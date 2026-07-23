//! Thin AppKit adapter over the renderer-independent `ViewModel`.
//!
//! Visual polish is deliberately deferred; application behavior belongs to `view_model`, side
//! effects to `app_controller`, and this module only translates AppKit interaction.

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    ptr::NonNull,
    sync::mpsc::Receiver,
};

use anyhow::{Context, Result};
use block2::{DynBlock, RcBlock};
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, Bool, ProtocolObject},
    sel,
};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertSecondButtonReturn, NSApplication,
    NSApplicationActivationPolicy, NSApplicationDelegate, NSAutoresizingMaskOptions,
    NSBackingStoreType, NSBezelStyle, NSBox, NSBoxType, NSButton, NSColor, NSControlStateValueOn,
    NSDatePicker, NSDatePickerElementFlags, NSDatePickerStyle, NSFont, NSImage, NSImageView,
    NSLayoutAttribute, NSMenu, NSMenuItem, NSPopUpButton, NSScrollView, NSSplitView,
    NSSplitViewDividerStyle, NSStackView, NSStackViewDistribution, NSStatusBar, NSStatusItem,
    NSTextAlignment, NSTextField, NSUserInterfaceLayoutOrientation, NSVariableStatusItemLength,
    NSView, NSWindow, NSWindowStyleMask,
};
use objc2_event_kit::{EKAuthorizationStatus, EKEntityType, EKEventStore};
use objc2_foundation::{
    NSArray, NSDate, NSEdgeInsets, NSError, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSet,
    NSSize, NSString, NSTimer,
};
use objc2_user_notifications::{
    UNAlertStyle, UNAuthorizationOptions, UNAuthorizationStatus, UNMutableNotificationContent,
    UNNotification, UNNotificationAction, UNNotificationActionOptionNone, UNNotificationCategory,
    UNNotificationCategoryOptionNone, UNNotificationDefaultActionIdentifier,
    UNNotificationInterruptionLevel, UNNotificationPresentationOptions, UNNotificationRequest,
    UNNotificationResponse, UNNotificationSetting, UNNotificationSettings, UNNotificationSound,
    UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};
use personal_audio_lake_contracts::{ActionKind, Envelope, Message, NotificationDelivery};
use uuid::Uuid;

use crate::{
    Config,
    app_controller::AppController,
    view_model::{
        ActionEditor, Automation, AutomationDraft, AutomationTiming, BackgroundOperationalState,
        IntegrationOption, Intent, Permission, PermissionState, Route, RoutingMode, Settings,
        ViewModel,
    },
};

#[derive(serde::Serialize)]
struct NativeNotificationSettings {
    authorization: &'static str,
    alerts_enabled: bool,
    sound_enabled: bool,
    notification_center_enabled: bool,
    time_sensitive_enabled: bool,
    alert_style: &'static str,
    managed_by: &'static str,
}

#[derive(serde::Serialize)]
struct NativeEventKitSettings {
    calendar: &'static str,
    reminders: &'static str,
    managed_by: &'static str,
}

const REVIEW_CATEGORY: &str = "PERSONAL_AUDIO_LAKE_RECORDING_REVIEW";
const REVIEW_CONFIRMED: &str = "PERSONAL_AUDIO_LAKE_REVIEW_CONFIRMED";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArrivalNotice {
    recording_id: Uuid,
    summary: String,
    hooks: String,
}

#[derive(Debug, Clone)]
pub(crate) enum AppEvent {
    Live(ArrivalNotice),
    CatchUp(Vec<ArrivalNotice>),
    ModelChanged,
}

impl From<&NotificationDelivery> for ArrivalNotice {
    fn from(delivery: &NotificationDelivery) -> Self {
        let hooks = if delivery.hooks.is_empty() {
            "Nothing was added".to_owned()
        } else {
            delivery
                .hooks
                .iter()
                .map(|hook| {
                    format!(
                        "{}: {} — {}",
                        action_kind(hook.kind),
                        hook.title,
                        notification_status(&hook.status)
                    )
                })
                .collect::<Vec<_>>()
                .join("; ")
        };
        Self {
            recording_id: delivery.recording_id,
            summary: delivery.summary.clone(),
            hooks,
        }
    }
}

fn notification_status(status: &str) -> String {
    match status {
        "delivered" => "Added".into(),
        "awaiting_review" | "pending" => "Waiting".into(),
        "rejected" | "skipped" => "Not Added".into(),
        "undone" | "removed" => "Undone".into(),
        value => sentence_case_words(value),
    }
}

fn sentence_case_words(value: &str) -> String {
    let value = value.replace('_', " ");
    let mut characters = value.chars();
    characters.next().map_or(value.clone(), |first| {
        first.to_uppercase().collect::<String>() + characters.as_str()
    })
}

struct MenuIvars {
    arrivals: RefCell<Receiver<AppEvent>>,
    controller: AppController,
    model: RefCell<ViewModel>,
    window: Retained<NSWindow>,
    sidebar: Retained<NSView>,
    split: Retained<NSSplitView>,
    list_pane: Retained<NSView>,
    sidebar_divider_position: f64,
    expanded_list_divider_position: f64,
    search: Retained<NSTextField>,
    list_document: Retained<NSView>,
    detail_document: Retained<NSView>,
    detail_section: RefCell<DetailSection>,
    technical_trace_expanded: RefCell<bool>,
    last_list_scroll_key: RefCell<Option<String>>,
    last_detail_scroll_key: RefCell<Option<String>>,
    open_menu_item: RefCell<Option<Retained<NSMenuItem>>>,
    source_status_menu_item: RefCell<Option<Retained<NSMenuItem>>>,
    sync_menu_item: RefCell<Option<Retained<NSMenuItem>>>,
    status_item: RefCell<Option<Retained<NSStatusItem>>>,
    status_content: RefCell<Option<Retained<StatusContentView>>>,
    status_icon: RefCell<Option<Retained<NSImageView>>>,
    status_badge: RefCell<Option<(Retained<NSBox>, Retained<NSTextField>)>>,
    catch_up_mode: RefCell<bool>,
    pending_notification: RefCell<Option<(Uuid, bool)>>,
    permission_poll_ticks: RefCell<u32>,
}

struct StatusContentIvars;

define_class!(
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = StatusContentIvars]
    struct StatusContentView;

    unsafe impl NSObjectProtocol for StatusContentView {}

    impl StatusContentView {
        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, _point: NSPoint) -> Option<Retained<NSView>> {
            // The status button owns the attached native menu. Decorative children must never
            // intercept either a primary or secondary click.
            None
        }
    }
);

impl StatusContentView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(StatusContentIvars);
        unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailSection {
    Overview,
    Transcript,
    Activity,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = MenuIvars]
    struct MenuDelegate;

    unsafe impl NSObjectProtocol for MenuDelegate {}

    unsafe impl NSApplicationDelegate for MenuDelegate {
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn application_should_handle_reopen(
            &self,
            _application: &NSApplication,
            _has_visible_windows: bool,
        ) -> bool {
            *self.ivars().catch_up_mode.borrow_mut() = false;
            self.dispatch(Intent::Navigate(Route::Review));
            self.show_window();
            true
        }
    }

    unsafe impl UNUserNotificationCenterDelegate for MenuDelegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion_handler: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion_handler.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion_handler: &DynBlock<dyn Fn()>,
        ) {
            if let Err(error) = self.handle_notification_response(response) {
                eprintln!("handle notification response: {error:#}");
            }
            completion_handler.call(());
        }
    }

    impl MenuDelegate {
        #[unsafe(method(openLake:))]
        fn open_lake(&self, _sender: &AnyObject) {
            let route = self
                .ivars()
                .model
                .borrow()
                .background_control_presentation()
                .primary_route;
            *self.ivars().catch_up_mode.borrow_mut() = false;
            self.dispatch(Intent::Navigate(route));
            self.show_window();
        }

        #[unsafe(method(showReview:))]
        fn show_review(&self, _sender: &AnyObject) {
            self.dispatch(Intent::Navigate(Route::Review));
        }

        #[unsafe(method(showLibrary:))]
        fn show_library(&self, _sender: &AnyObject) {
            self.dispatch(Intent::Navigate(Route::Library));
        }

        #[unsafe(method(showAutomations:))]
        fn show_automations(&self, _sender: &AnyObject) {
            self.dispatch(Intent::Navigate(Route::Automations));
        }

        #[unsafe(method(selectRecording:))]
        fn select_recording(&self, sender: &AnyObject) {
            if let Some(button) = sender.downcast_ref::<NSButton>()
                && button.tag() >= 0
            {
                self.dispatch(Intent::SelectVisible(button.tag() as usize));
            }
        }

        #[unsafe(method(searchLibrary:))]
        fn search_library(&self, _sender: &AnyObject) {
            self.dispatch(Intent::Search(self.ivars().search.stringValue().to_string()));
        }

        #[unsafe(method(refreshData:))]
        fn refresh_data(&self, _sender: &AnyObject) {
            self.dispatch(Intent::Refresh);
        }

        #[unsafe(method(playAudio:))]
        fn play_audio(&self, _sender: &AnyObject) {
            self.dispatch(Intent::PlayAudio);
        }

        #[unsafe(method(playTranscriptSegment:))]
        fn play_transcript_segment(&self, sender: &AnyObject) {
            if let Some(button) = sender.downcast_ref::<NSButton>() && button.tag() >= 0 {
                self.dispatch(Intent::PlayTranscriptSegment(button.tag() as u32));
            }
        }

        #[unsafe(method(renameRecording:))]
        fn rename_recording(&self, _sender: &AnyObject) {
            let current = self.ivars().model.borrow().recording_presentation()
                .map(|detail| detail.header.title).unwrap_or_default();
            if let Some(title) = self.prompt_with_value("Rename recording", "Use a short title you will recognise later.", &current) {
                self.dispatch(Intent::RenameRecording(title));
            }
        }

        #[unsafe(method(showOverview:))]
        fn show_overview(&self, _sender: &AnyObject) {
            *self.ivars().detail_section.borrow_mut() = DetailSection::Overview;
            self.render();
        }

        #[unsafe(method(showTranscript:))]
        fn show_transcript(&self, _sender: &AnyObject) {
            *self.ivars().detail_section.borrow_mut() = DetailSection::Transcript;
            self.render();
        }

        #[unsafe(method(showActivity:))]
        fn show_activity(&self, _sender: &AnyObject) {
            *self.ivars().detail_section.borrow_mut() = DetailSection::Activity;
            self.render();
        }

        #[unsafe(method(toggleTechnicalTrace:))]
        fn toggle_technical_trace(&self, _sender: &AnyObject) {
            let expanded = *self.ivars().technical_trace_expanded.borrow();
            *self.ivars().technical_trace_expanded.borrow_mut() = !expanded;
            self.render();
        }

        #[unsafe(method(correctTranscriptSegment:))]
        fn correct_transcript_segment(&self, sender: &AnyObject) {
            let Some(button) = sender.downcast_ref::<NSButton>() else { return; };
            let ordinal = button.tag() as u32;
            let current = self.ivars().model.borrow().transcript_text(ordinal).unwrap_or_default();
            if let Some(replacement) = self.prompt_with_value(
                "Correct transcript", "The raw transcript is retained; this saves a correction overlay.", &current,
            ) {
                self.dispatch(Intent::CorrectTranscript { ordinal, replacement });
            }
        }

        #[unsafe(method(approveAction:))]
        fn approve_action(&self, sender: &AnyObject) {
            if let Some(button) = sender.downcast_ref::<NSButton>() && button.tag() >= 0 {
                self.dispatch(Intent::ApproveAction(button.tag() as usize));
            }
        }

        #[unsafe(method(rejectAction:))]
        fn reject_action(&self, sender: &AnyObject) {
            if let Some(button) = sender.downcast_ref::<NSButton>()
                && button.tag() >= 0
                && let Some(reason) =
                    self.prompt("Do not run this action", "Why should the app skip it?")
            {
                self.dispatch(Intent::RejectAction {
                    index: button.tag() as usize,
                    reason,
                });
            }
        }

        #[unsafe(method(removeAction:))]
        fn remove_action(&self, sender: &AnyObject) {
            if let Some(button) = sender.downcast_ref::<NSButton>() && button.tag() >= 0 {
                self.dispatch(Intent::RemoveAction(button.tag() as usize));
            }
        }

        #[unsafe(method(addAction:))]
        fn add_action(&self, _sender: &AnyObject) {
            let editor = match self.ivars().model.borrow().action_editor(None) {
                Ok(editor) => editor,
                Err(error) => {
                    self.show_error(&error);
                    return;
                }
            };
            if let Some((kind, title, due_at_ms)) = self.prompt_action(&editor) {
                self.dispatch(Intent::AddAction {
                    kind,
                    title,
                    due_at_ms,
                });
            }
        }

        #[unsafe(method(editAction:))]
        fn edit_action(&self, sender: &AnyObject) {
            let Some(button) = sender.downcast_ref::<NSButton>() else {
                return;
            };
            let index = button.tag();
            if index < 0 {
                return;
            }
            let editor = match self
                .ivars()
                .model
                .borrow()
                .action_editor(Some(index as usize))
            {
                Ok(editor) => editor,
                Err(error) => {
                    self.show_error(&error);
                    return;
                }
            };
            if let Some((kind, title, due_at_ms)) = self.prompt_action(&editor) {
                self.dispatch(Intent::EditAction {
                    index: index as usize,
                    kind,
                    title,
                    due_at_ms,
                });
            }
        }

        #[unsafe(method(confirmRecording:))]
        fn confirm_recording(&self, _sender: &AnyObject) {
            self.dispatch(Intent::ConfirmProcessing);
        }

        #[unsafe(method(correctTranscript:))]
        fn correct_transcript(&self, _sender: &AnyObject) {
            self.correct_transcript_flow();
        }

        #[unsafe(method(operateWorkflowRun:))]
        fn operate_workflow_run(&self, sender: &AnyObject) {
            let Some(button) = sender.downcast_ref::<NSButton>() else {
                return;
            };
            let index = button.tag();
            if index < 0 {
                return;
            }
            let run_index = index as usize;
            let editor = match self
                .ivars()
                .model
                .borrow()
                .workflow_correction_editor(run_index)
            {
                Ok(editor) => editor,
                Err(error) => {
                    self.show_error(&error);
                    return;
                }
            };
            let alert = NSAlert::new(self.mtm());
            alert.setMessageText(&NSString::from_str(&editor.heading));
            alert.setInformativeText(&NSString::from_str(&editor.explanation));
            let form = vertical_stack(self.mtm(), 8.0, 0.0);
            form.setFrame(rect(0.0, 0.0, 520.0, 80.0));
            let mut inputs = Vec::new();
            for field in &editor.fields {
                form.addArrangedSubview(&stack_label(
                    self.mtm(),
                    &format!(
                        "{}{}",
                        field.label,
                        if field.required { " (required)" } else { "" }
                    ),
                    12.0,
                    true,
                    520.0,
                ));
                let input = NSTextField::initWithFrame(
                    NSTextField::alloc(self.mtm()),
                    rect(0.0, 0.0, 520.0, 28.0),
                );
                input.setStringValue(&NSString::from_str(&field.value));
                form.addArrangedSubview(&input);
                inputs.push(input);
            }
            if !editor.fields.is_empty() {
                form.layoutSubtreeIfNeeded();
                form.setFrameSize(NSSize::new(520.0, form.fittingSize().height.max(40.0)));
                alert.setAccessoryView(Some(&form));
                alert.addButtonWithTitle(&NSString::from_str(&editor.apply_label));
            }
            if editor.can_undo {
                alert.addButtonWithTitle(&NSString::from_str("Undo This Change"));
            }
            alert.addButtonWithTitle(&NSString::from_str("Cancel"));
            let response = alert.runModal();
            if !editor.fields.is_empty() && response == NSAlertFirstButtonReturn {
                let outputs = editor
                    .fields
                    .iter()
                    .zip(inputs)
                    .map(|(field, input)| (field.name.clone(), input.stringValue().to_string()))
                    .collect::<BTreeMap<_, _>>();
                self.dispatch(Intent::AdjustWorkflowRun {
                    run_index: editor.run_index,
                    state_id: editor.state_id,
                    transition_id: editor.transition_id,
                    outputs,
                });
            } else if editor.can_undo
                && ((!editor.fields.is_empty() && response == NSAlertSecondButtonReturn)
                    || (editor.fields.is_empty() && response == NSAlertFirstButtonReturn))
            {
                self.dispatch(Intent::UndoLatestWorkflowEffect(run_index));
            }
        }

        #[unsafe(method(runWorkflow:))]
        fn run_workflow(&self, sender: &AnyObject) {
            if let Some(button) = sender.downcast_ref::<NSButton>()
                && button.tag() >= 0
            {
                self.dispatch(Intent::RunWorkflowWithLocalModel(button.tag() as usize));
            }
        }

        #[unsafe(method(routeWithLocalModel:))]
        fn route_with_local_model(&self, _sender: &AnyObject) {
            self.dispatch(Intent::RouteWithLocalModel);
        }

        #[unsafe(method(toggleAutomation:))]
        fn toggle_automation(&self, sender: &AnyObject) {
            if let Some(button) = sender.downcast_ref::<NSButton>()
                && button.tag() >= 0
            {
                self.dispatch(Intent::ToggleAutomation(button.tag() as usize));
            }
        }

        #[unsafe(method(toggleWorkflow:))]
        fn toggle_workflow(&self, sender: &AnyObject) {
            if let Some(button) = sender.downcast_ref::<NSButton>()
                && button.tag() >= 0
            {
                self.dispatch(Intent::ToggleWorkflow(button.tag() as usize));
            }
        }

        #[unsafe(method(createAutomation:))]
        fn create_automation(&self, _sender: &AnyObject) {
            if let Some(value) = self.prompt(
                "Create routing rule",
                "id|name|explicit, hybrid, or implicit|trigger phrases separated by /|destination",
            ) {
                match parse_automation(&value) {
                    Ok(automation) => self.dispatch(Intent::CreateAutomation(automation)),
                    Err(error) => self.show_error(&error),
                }
            }
        }

        #[unsafe(method(createWorkflow:))]
        fn create_workflow(&self, _sender: &AnyObject) {
            let Some(name) = self.prompt("New Automation", "What would you like to call it?") else {
                return;
            };
            let Some(description) = self.prompt(
                "What should it do?",
                "Describe when this automation is relevant and what the connected action does.",
            ) else {
                return;
            };
            let Some(when_relevant) = self.choose(
                "When should it run?",
                "The app can use the local model to decide whether each recording is relevant, or run the automation for every recording.",
                "When It’s Relevant",
                "Every Recording",
            ) else {
                return;
            };
            let Some(integration) = self.choose_integration() else {
                return;
            };
            let target = match (&integration.target_heading, &integration.target_help) {
                (Some(heading), Some(help)) => {
                    let Some(target) = self.prompt(heading, help) else {
                        return;
                    };
                    target
                }
                _ => String::new(),
            };
            let undo_target = match (&integration.undo_heading, &integration.undo_help) {
                (Some(heading), Some(help)) => {
                    let Some(target) = self.prompt_optional(heading, help) else {
                        return;
                    };
                    target
                }
                _ => None,
            };
            self.dispatch(Intent::CreateWorkflowFromDraft(AutomationDraft {
                name,
                description,
                timing: if when_relevant {
                    AutomationTiming::WhenRelevant
                } else {
                    AutomationTiming::EveryRecording
                },
                connection: integration.connection,
                target,
                undo_target,
            }));
        }

        #[unsafe(method(requestNotificationAccess:))]
        fn request_notification_access_button(&self, _sender: &AnyObject) {
            self.request_permission_flow(Permission::Notifications);
        }

        #[unsafe(method(requestCalendarAccess:))]
        fn request_calendar_access_button(&self, _sender: &AnyObject) {
            self.request_permission_flow(Permission::Calendar);
        }

        #[unsafe(method(requestRemindersAccess:))]
        fn request_reminders_access_button(&self, _sender: &AnyObject) {
            self.request_permission_flow(Permission::Reminders);
        }

        #[unsafe(method(syncNow:))]
        fn sync_now(&self, _sender: &AnyObject) {
            if self
                .ivars()
                .model
                .borrow()
                .background_control_presentation()
                .source_setup_required
            {
                self.dispatch(Intent::Navigate(Route::Settings));
                self.show_window();
            } else {
                self.dispatch(Intent::Sync);
            }
        }

        #[unsafe(method(selectSourceAdapter:))]
        fn select_source_adapter(&self, sender: &AnyObject) {
            if let Some(button) = sender.downcast_ref::<NSButton>() && button.tag() >= 0 {
                self.dispatch(Intent::SelectSourceAdapter(button.tag() as usize));
            }
        }

        #[unsafe(method(releaseSource:))]
        fn release_source(&self, _sender: &AnyObject) {
            self.dispatch(Intent::ReleaseSource);
        }

        #[unsafe(method(openLakeSettings:))]
        fn open_lake_settings(&self, _sender: &AnyObject) {
            self.dispatch(Intent::Navigate(Route::Settings));
            self.show_window();
        }

        #[unsafe(method(pollArrivals:))]
        fn poll_arrivals(&self, _timer: &AnyObject) {
            self.poll_app_events();
        }
    }
);

impl MenuDelegate {
    // AppKit constructs these retained views together; keeping the ownership transfer explicit is
    // easier to audit than hiding native objects in an untyped collection.
    #[allow(clippy::too_many_arguments)]
    fn new(
        mtm: MainThreadMarker,
        controller: AppController,
        arrivals: Receiver<AppEvent>,
        model: ViewModel,
        window: Retained<NSWindow>,
        sidebar: Retained<NSView>,
        split: Retained<NSSplitView>,
        list_pane: Retained<NSView>,
        search: Retained<NSTextField>,
        list_document: Retained<NSView>,
        detail_document: Retained<NSView>,
    ) -> Retained<Self> {
        let sidebar_frame = sidebar.frame();
        let sidebar_divider_position = sidebar_frame.origin.x + sidebar_frame.size.width;
        let list_frame = list_pane.frame();
        let expanded_list_divider_position = list_frame.origin.x + list_frame.size.width;
        let this = Self::alloc(mtm).set_ivars(MenuIvars {
            arrivals: RefCell::new(arrivals),
            controller,
            model: RefCell::new(model),
            window,
            sidebar,
            split,
            list_pane,
            sidebar_divider_position,
            expanded_list_divider_position,
            search,
            list_document,
            detail_document,
            detail_section: RefCell::new(DetailSection::Overview),
            technical_trace_expanded: RefCell::new(false),
            last_list_scroll_key: RefCell::new(None),
            last_detail_scroll_key: RefCell::new(None),
            open_menu_item: RefCell::new(None),
            source_status_menu_item: RefCell::new(None),
            sync_menu_item: RefCell::new(None),
            status_item: RefCell::new(None),
            status_content: RefCell::new(None),
            status_icon: RefCell::new(None),
            status_badge: RefCell::new(None),
            catch_up_mode: RefCell::new(false),
            pending_notification: RefCell::new(None),
            permission_poll_ticks: RefCell::new(0),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.render();
        this
    }

    fn dispatch(&self, intent: Intent) {
        let mut model = self.ivars().model.borrow_mut();
        let error = match model.update(intent) {
            Ok(effects) => {
                if let Err(error) = self.ivars().controller.apply_all(effects, &mut model) {
                    Some(format!("{error:#}"))
                } else {
                    None
                }
            }
            Err(error) => Some(error),
        };
        drop(model);
        self.render();
        if let Some(error) = error {
            self.show_error(&error);
        }
    }

    fn render(&self) {
        let model = self.ivars().model.borrow();
        let route = model.route();
        let shows_recording_list = matches!(route, Route::Review | Route::Library);
        // NSSplitView can close the containing window when an arranged subview is hidden while
        // handling that subview's own button event. Keep the pane in the split hierarchy and
        // collapse it with the divider instead.
        self.ivars().list_pane.setHidden(false);
        self.ivars().split.adjustSubviews();
        self.ivars()
            .split
            .setPosition_ofDividerAtIndex(self.ivars().sidebar_divider_position, 0);
        self.ivars().split.setPosition_ofDividerAtIndex(
            if shows_recording_list {
                self.ivars().expanded_list_divider_position
            } else {
                self.ivars().sidebar_divider_position
            },
            1,
        );
        self.ivars().search.setHidden(route != Route::Library);
        self.render_recording_list(&model);
        self.render_detail(&model);
        let window_title = if route == Route::Review && *self.ivars().catch_up_mode.borrow() {
            "Captain’s Log — While you were away".to_owned()
        } else {
            format!("Captain’s Log — {}", route.label())
        };
        self.ivars()
            .window
            .setTitle(&NSString::from_str(&window_title));
        let menu_bar = model.background_control_presentation();
        if let Some(item) = self.ivars().open_menu_item.borrow().as_ref() {
            item.setTitle(&NSString::from_str(&menu_bar.primary_action));
        }
        if let Some(item) = self.ivars().source_status_menu_item.borrow().as_ref() {
            item.setTitle(&NSString::from_str(&menu_bar.source_status));
        }
        if let Some(item) = self.ivars().sync_menu_item.borrow().as_ref() {
            item.setTitle(&NSString::from_str(&menu_bar.sync_action));
            item.setEnabled(menu_bar.can_sync);
        }
        self.render_status_icon(menu_bar.operational_state, &menu_bar.source_status);
        self.render_status_badge(model.inbox_badge_text().as_deref());
    }

    fn render_status_icon(&self, state: BackgroundOperationalState, source_status: &str) {
        let symbol = match state {
            BackgroundOperationalState::Idle => "waveform.circle.fill",
            BackgroundOperationalState::Active => "arrow.triangle.2.circlepath.circle.fill",
            BackgroundOperationalState::NeedsAttention => "exclamationmark.triangle.fill",
        };
        if let Some(icon) = self.ivars().status_icon.borrow().as_ref()
            && let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str(symbol),
                Some(&NSString::from_str(source_status)),
            )
        {
            image.setTemplate(true);
            icon.setImage(Some(&image));
        }
        if let Some(status_item) = self.ivars().status_item.borrow().as_ref()
            && let Some(button) = status_item.button(self.mtm())
        {
            button.setToolTip(Some(&NSString::from_str(&format!(
                "Captain’s Log — {source_status}"
            ))));
        }
    }

    fn render_status_badge(&self, badge_text: Option<&str>) {
        let Some(status_item) = self.ivars().status_item.borrow().as_ref().cloned() else {
            return;
        };
        let Some(content) = self.ivars().status_content.borrow().as_ref().cloned() else {
            return;
        };
        let Some((badge, label)) = self.ivars().status_badge.borrow().as_ref().cloned() else {
            return;
        };

        let badge_width = badge_text.map(status_badge_width).unwrap_or(0.0);
        let item_width = if badge_text.is_some() {
            25.0 + badge_width
        } else {
            28.0
        };
        status_item.setLength(item_width);
        content.setFrame(rect(0.0, 0.0, item_width, 24.0));
        if let Some(text) = badge_text {
            badge.setFrame(rect(20.0, 8.0, badge_width, 15.0));
            label.setFrame(rect(0.0, 0.0, badge_width, 15.0));
            label.setStringValue(&NSString::from_str(text));
            badge.setHidden(false);
        } else {
            badge.setHidden(true);
        }
        status_item.setVisible(true);
    }

    fn render_recording_list(&self, model: &ViewModel) {
        clear_view(&self.ivars().list_document);
        let rows = model.library_rows();
        let height = (rows.len() as f64 * 76.0 + 24.0).max(620.0);
        let width = viewport_width(&self.ivars().list_document, 220.0);
        self.ivars()
            .list_document
            .setFrameSize(NSSize::new(width, height));
        let scroll_key = format!("{:?}:{}", model.route(), model.library_query());
        scroll_to_top_if_changed(
            &self.ivars().list_document,
            height,
            scroll_key,
            &self.ivars().last_list_scroll_key,
        );
        if rows.is_empty() {
            add_label(
                &self.ivars().list_document,
                match model.route() {
                    Route::Review => "You’re All Caught Up",
                    Route::Library => "No Recordings Found",
                    Route::Automations | Route::Settings => "",
                },
                rect(18.0, height - 54.0, width - 36.0, 32.0),
                14.0,
                false,
            );
            return;
        }
        for (index, row) in rows.iter().enumerate() {
            let button = app_button(
                self.mtm(),
                &format!("{}\n{}  ·  {}", row.title, row.subtitle, row.outcome),
                sel!(selectRecording:),
                self,
            );
            button.setTag(index as isize);
            button.setBezelStyle(NSBezelStyle::FlexiblePush);
            button.setFrame(rect(
                10.0,
                height - 72.0 - index as f64 * 76.0,
                width - 20.0,
                66.0,
            ));
            self.ivars().list_document.addSubview(&button);
        }
    }

    fn render_detail(&self, model: &ViewModel) {
        clear_view(&self.ivars().detail_document);
        match model.route() {
            Route::Review | Route::Library => self.render_recording_detail(model),
            Route::Automations => self.render_automations(model),
            Route::Settings => self.render_settings(model),
        }
        let height = self.ivars().detail_document.frame().size.height;
        let scroll_key = format!(
            "{:?}:{:?}:{:?}:{}",
            model.route(),
            model.selected_id(),
            *self.ivars().detail_section.borrow(),
            *self.ivars().technical_trace_expanded.borrow(),
        );
        scroll_to_top_if_changed(
            &self.ivars().detail_document,
            height,
            scroll_key,
            &self.ivars().last_detail_scroll_key,
        );
    }

    fn render_recording_detail(&self, model: &ViewModel) {
        let viewport = viewport_size(&self.ivars().detail_document, 420.0, 600.0);
        let content_width = viewport.width - 48.0;
        let root = vertical_stack(self.mtm(), 16.0, 24.0);
        let Some(detail) = model.recording_presentation() else {
            root.addArrangedSubview(&stack_label(
                self.mtm(),
                "Select a recording",
                24.0,
                true,
                content_width,
            ));
            mount_stack(&self.ivars().detail_document, &root, viewport);
            return;
        };

        root.addArrangedSubview(&stack_label(
            self.mtm(),
            &detail.header.title,
            25.0,
            true,
            content_width,
        ));

        let metadata = horizontal_stack(self.mtm(), 12.0, NSLayoutAttribute::CenterY);
        metadata.addArrangedSubview(&stack_label(
            self.mtm(),
            &format!(
                "{}  ·  {}",
                detail.header.metadata, detail.header.review_status
            ),
            13.0,
            false,
            content_width,
        ));
        let rename = app_button(self.mtm(), "Rename…", sel!(renameRecording:), self);
        metadata.addArrangedSubview(&rename);
        root.addArrangedSubview(&metadata);

        if let Some(review) = &detail.review_acknowledgement {
            let content = vertical_stack(self.mtm(), 10.0, 0.0);
            content.addArrangedSubview(&stack_label(
                self.mtm(),
                &review.explanation,
                14.0,
                false,
                content_width,
            ));
            let reviewed = app_button(
                self.mtm(),
                &review.action_label,
                sel!(confirmRecording:),
                self,
            );
            content.addArrangedSubview(&reviewed);
            root.addArrangedSubview(&receipt_box(self.mtm(), &review.heading, &content));
        }

        let tabs = horizontal_stack(self.mtm(), 8.0, NSLayoutAttribute::CenterY);
        for (title, selector) in [
            ("Overview", sel!(showOverview:)),
            ("Transcript", sel!(showTranscript:)),
            ("History", sel!(showActivity:)),
        ] {
            let button = app_button(self.mtm(), title, selector, self);
            tabs.addArrangedSubview(&button);
        }
        root.addArrangedSubview(&tabs);

        match *self.ivars().detail_section.borrow() {
            DetailSection::Overview => self.render_overview(model, &detail, &root, content_width),
            DetailSection::Transcript => self.render_transcript(&detail, &root, content_width),
            DetailSection::Activity => self.render_activity(&detail, &root, content_width),
        }
        mount_stack(&self.ivars().detail_document, &root, viewport);
    }

    fn render_overview(
        &self,
        model: &ViewModel,
        detail: &crate::view_model::RecordingPresentation,
        root: &NSStackView,
        content_width: f64,
    ) {
        root.addArrangedSubview(&stack_label(
            self.mtm(),
            "What the app heard",
            18.0,
            true,
            content_width,
        ));
        root.addArrangedSubview(&stack_label(
            self.mtm(),
            &detail.summary,
            15.0,
            false,
            content_width,
        ));
        root.addArrangedSubview(&stack_label(
            self.mtm(),
            "Actions Taken",
            18.0,
            true,
            content_width,
        ));
        if let Some(message) = &detail.no_action_message {
            let content = vertical_stack(self.mtm(), 8.0, 0.0);
            add_receipt_field(
                self.mtm(),
                &content,
                "Changes",
                "Nothing was added",
                content_width,
            );
            add_receipt_field(self.mtm(), &content, "Reason", message, content_width);
            root.addArrangedSubview(&receipt_box(self.mtm(), "Nothing Added", &content));
        }
        for action in &detail.actions {
            let content = vertical_stack(self.mtm(), 10.0, 0.0);
            add_receipt_field(
                self.mtm(),
                &content,
                "Action",
                &action.operation,
                content_width,
            );
            add_receipt_field(
                self.mtm(),
                &content,
                &action.payload_label,
                &action.payload,
                content_width,
            );
            if let Some(evidence) = &action.evidence_quote {
                add_receipt_field(
                    self.mtm(),
                    &content,
                    "Why this action was chosen",
                    evidence,
                    content_width,
                );
            }
            if let Some(scheduled_for) = &action.scheduled_for {
                add_receipt_field(self.mtm(), &content, "When", scheduled_for, content_width);
            }
            add_receipt_field(
                self.mtm(),
                &content,
                "Destination",
                &action.destination,
                content_width,
            );
            add_receipt_field(
                self.mtm(),
                &content,
                "Status",
                &action.state_label,
                content_width,
            );
            add_receipt_field(
                self.mtm(),
                &content,
                "Result",
                &action.state_detail,
                content_width,
            );
            if action.can_decide {
                let choices = horizontal_stack(self.mtm(), 8.0, NSLayoutAttribute::CenterY);
                let approve = app_button(self.mtm(), "Run Now", sel!(approveAction:), self);
                approve.setTag(action.index as isize);
                choices.addArrangedSubview(&approve);
                content.addArrangedSubview(&choices);
            }
            let edit = app_button(self.mtm(), &action.edit_label, sel!(editAction:), self);
            edit.setTag(action.index as isize);
            content.addArrangedSubview(&edit);
            if let Some(label) = &action.remove_label {
                let remove = app_button(self.mtm(), label, sel!(removeAction:), self);
                remove.setTag(action.index as isize);
                content.addArrangedSubview(&remove);
            }
            root.addArrangedSubview(&receipt_box(
                self.mtm(),
                &format!("Action {}", action.index + 1),
                &content,
            ));
        }

        let action_commands = horizontal_stack(self.mtm(), 8.0, NSLayoutAttribute::CenterY);
        let add_action = app_button(self.mtm(), "Add an Action…", sel!(addAction:), self);
        action_commands.addArrangedSubview(&add_action);
        let route = app_button(
            self.mtm(),
            "Ask the App to Check Again",
            sel!(routeWithLocalModel:),
            self,
        );
        action_commands.addArrangedSubview(&route);
        root.addArrangedSubview(&action_commands);

        let commands = horizontal_stack(self.mtm(), 8.0, NSLayoutAttribute::CenterY);
        if detail.header.audio_available {
            let play = app_button(self.mtm(), "Play recording", sel!(playAudio:), self);
            commands.addArrangedSubview(&play);
        }
        if detail.header.audio_available {
            root.addArrangedSubview(&commands);
        }

        for workflow in model
            .workflow_rows()
            .into_iter()
            .filter(|workflow| workflow.enabled)
        {
            let button = app_button(self.mtm(), &workflow.run_label, sel!(runWorkflow:), self);
            button.setTag(workflow.index as isize);
            root.addArrangedSubview(&button);
        }
    }

    fn render_transcript(
        &self,
        detail: &crate::view_model::RecordingPresentation,
        root: &NSStackView,
        content_width: f64,
    ) {
        root.addArrangedSubview(&stack_label(
            self.mtm(),
            "Transcript",
            18.0,
            true,
            content_width,
        ));
        for segment in &detail.transcript {
            let item = vertical_stack(self.mtm(), 8.0, 0.0);
            let controls = horizontal_stack(self.mtm(), 8.0, NSLayoutAttribute::CenterY);
            let play = app_button(
                self.mtm(),
                &format!("▶ {}", segment.time_label),
                sel!(playTranscriptSegment:),
                self,
            );
            play.setTag(segment.ordinal as isize);
            controls.addArrangedSubview(&play);
            let edit = app_button(self.mtm(), "Edit…", sel!(correctTranscriptSegment:), self);
            edit.setTag(segment.ordinal as isize);
            controls.addArrangedSubview(&edit);
            item.addArrangedSubview(&controls);
            item.addArrangedSubview(&stack_label(
                self.mtm(),
                &format!(
                    "{}{}",
                    segment.text,
                    if segment.corrected {
                        "  ·  Corrected"
                    } else {
                        ""
                    }
                ),
                14.0,
                false,
                content_width,
            ));
            root.addArrangedSubview(&item);
        }
    }

    fn render_activity(
        &self,
        detail: &crate::view_model::RecordingPresentation,
        root: &NSStackView,
        content_width: f64,
    ) {
        root.addArrangedSubview(&stack_label(
            self.mtm(),
            "Automation History",
            18.0,
            true,
            content_width,
        ));
        if detail.activity.is_empty() {
            root.addArrangedSubview(&stack_label(
                self.mtm(),
                "No automations ran for this recording.",
                14.0,
                false,
                content_width,
            ));
        }
        for receipt in &detail.activity {
            let content = vertical_stack(self.mtm(), 10.0, 0.0);
            add_receipt_field(
                self.mtm(),
                &content,
                "Automation",
                &receipt.workflow,
                content_width,
            );
            add_receipt_field(
                self.mtm(),
                &content,
                "Started",
                &receipt.triggered_by,
                content_width,
            );
            add_receipt_field(
                self.mtm(),
                &content,
                "Status",
                &receipt.state,
                content_width,
            );
            add_receipt_field(
                self.mtm(),
                &content,
                "Outcome",
                &receipt.result,
                content_width,
            );
            add_receipt_field(
                self.mtm(),
                &content,
                "Checked",
                &receipt.confirmation,
                content_width,
            );
            if receipt.can_adjust {
                let button = app_button(
                    self.mtm(),
                    "Correct or Undo…",
                    sel!(operateWorkflowRun:),
                    self,
                );
                button.setTag(receipt.run_index as isize);
                content.addArrangedSubview(&button);
            }
            root.addArrangedSubview(&receipt_box(self.mtm(), &receipt.workflow, &content));
        }
        let expanded = *self.ivars().technical_trace_expanded.borrow();
        let toggle = app_button(
            self.mtm(),
            if expanded {
                "Hide Details"
            } else {
                "Show Details"
            },
            sel!(toggleTechnicalTrace:),
            self,
        );
        root.addArrangedSubview(&toggle);
        if expanded && !detail.technical_trace.is_empty() {
            root.addArrangedSubview(&stack_label(
                self.mtm(),
                &detail.technical_trace,
                12.0,
                false,
                content_width,
            ));
        }
    }

    fn render_automations(&self, model: &ViewModel) {
        let viewport = viewport_size(&self.ivars().detail_document, 420.0, 600.0);
        let content_width = viewport.width - 48.0;
        let root = vertical_stack(self.mtm(), 16.0, 24.0);
        root.addArrangedSubview(&stack_label(
            self.mtm(),
            "Automations",
            25.0,
            true,
            content_width,
        ));
        root.addArrangedSubview(&stack_label(
            self.mtm(),
            "The app checks every new recording and can create items in your Mac apps automatically. Every integration is a configurable state machine and every native change can be undone.",
            14.0,
            false,
            content_width,
        ));
        let rows = model.workflow_rows();
        if rows.is_empty() {
            root.addArrangedSubview(&stack_label(
                self.mtm(),
                "The app is still loading your automations.",
                14.0,
                false,
                content_width,
            ));
        }
        for row in rows {
            let content = vertical_stack(self.mtm(), 10.0, 0.0);
            add_receipt_field(
                self.mtm(),
                &content,
                "What it does",
                &row.description,
                content_width,
            );
            add_receipt_field(
                self.mtm(),
                &content,
                "Status",
                &row.status_label,
                content_width,
            );
            let button = app_button(self.mtm(), &row.toggle_label, sel!(toggleWorkflow:), self);
            button.setTag(row.index as isize);
            content.addArrangedSubview(&button);
            root.addArrangedSubview(&receipt_box(self.mtm(), &row.name, &content));
        }
        let create = app_button(self.mtm(), "New Automation…", sel!(createWorkflow:), self);
        root.addArrangedSubview(&create);
        root.addArrangedSubview(&stack_label(
            self.mtm(),
            "Native integrations include Calendar, Reminders, Notes, Mail drafts, Contacts, and text files. Shortcuts connects Messages, Home, Maps, Music, and other apps; webhooks connect external services.",
            12.0,
            false,
            content_width,
        ));
        mount_stack(&self.ivars().detail_document, &root, viewport);
    }

    fn render_settings(&self, model: &ViewModel) {
        let viewport = viewport_size(&self.ivars().detail_document, 420.0, 600.0);
        let content_width = viewport.width - 48.0;
        let root = vertical_stack(self.mtm(), 16.0, 24.0);
        let settings = model.settings_presentation();
        root.addArrangedSubview(&stack_label(
            self.mtm(),
            "Settings",
            25.0,
            true,
            content_width,
        ));

        let transfer = vertical_stack(self.mtm(), 10.0, 0.0);
        add_receipt_field(
            self.mtm(),
            &transfer,
            "Status",
            &settings.recording_transfer_status,
            content_width,
        );
        add_receipt_field(
            self.mtm(),
            &transfer,
            "Service",
            &settings.recording_transfer_detail,
            content_width,
        );
        transfer.addArrangedSubview(&stack_label(
            self.mtm(),
            &settings.recording_transfer_help,
            12.0,
            false,
            content_width,
        ));
        root.addArrangedSubview(&receipt_box(self.mtm(), "Recorder App Transfer", &transfer));

        let device = vertical_stack(self.mtm(), 10.0, 0.0);
        add_receipt_field(
            self.mtm(),
            &device,
            "Device",
            &settings.source_name,
            content_width,
        );
        add_receipt_field(
            self.mtm(),
            &device,
            "Connection",
            &settings.connection,
            content_width,
        );
        add_receipt_field(
            self.mtm(),
            &device,
            "Open at Login",
            &settings.open_at_login,
            content_width,
        );
        if !settings.source_options.is_empty() {
            for (index, option) in settings.source_options.iter().enumerate() {
                add_receipt_field(
                    self.mtm(),
                    &device,
                    &option.name,
                    &option.description,
                    content_width,
                );
                let button = app_button(
                    self.mtm(),
                    if option.selected {
                        "Selected"
                    } else {
                        "Use This Source"
                    },
                    sel!(selectSourceAdapter:),
                    self,
                );
                button.setTag(index as isize);
                button.setEnabled(!option.selected);
                device.addArrangedSubview(&button);
            }
        }
        if !settings.source_setup_required {
            let sync = app_button(self.mtm(), "Sync Source", sel!(syncNow:), self);
            device.addArrangedSubview(&sync);
        }
        if settings.can_release_source {
            let release = app_button(self.mtm(), "Release source", sel!(releaseSource:), self);
            device.addArrangedSubview(&release);
        }
        root.addArrangedSubview(&receipt_box(self.mtm(), "Compatibility Device", &device));

        let privacy = vertical_stack(self.mtm(), 10.0, 0.0);
        add_receipt_field(
            self.mtm(),
            &privacy,
            "Processing",
            &settings.processing,
            content_width,
        );
        add_receipt_field(
            self.mtm(),
            &privacy,
            "Storage Location",
            &settings.storage,
            content_width,
        );
        root.addArrangedSubview(&receipt_box(self.mtm(), "Privacy & Storage", &privacy));

        root.addArrangedSubview(&stack_label(
            self.mtm(),
            "Mac Permissions",
            18.0,
            true,
            content_width,
        ));
        for permission in settings.permissions {
            let content = vertical_stack(self.mtm(), 8.0, 0.0);
            add_receipt_field(
                self.mtm(),
                &content,
                "Status",
                &permission.status,
                content_width,
            );
            if let Some(action_label) = &permission.action_label {
                let selector = match permission.permission {
                    Permission::Notifications => sel!(requestNotificationAccess:),
                    Permission::Calendar => sel!(requestCalendarAccess:),
                    Permission::Reminders => sel!(requestRemindersAccess:),
                };
                let button = app_button(self.mtm(), action_label, selector, self);
                content.addArrangedSubview(&button);
            }
            root.addArrangedSubview(&receipt_box(self.mtm(), &permission.name, &content));
        }
        mount_stack(&self.ivars().detail_document, &root, viewport);
    }

    fn prompt(&self, title: &str, explanation: &str) -> Option<String> {
        self.prompt_with_value(title, explanation, "")
    }

    fn prompt_action(&self, editor: &ActionEditor) -> Option<(ActionKind, String, Option<i64>)> {
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&NSString::from_str(&editor.heading));
        alert.setInformativeText(&NSString::from_str(&editor.explanation));

        let form = vertical_stack(self.mtm(), 6.0, 0.0);
        form.setFrame(rect(0.0, 0.0, 480.0, 180.0));
        form.addArrangedSubview(&stack_label(self.mtm(), "Action type", 12.0, true, 480.0));
        let kind = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(self.mtm()),
            rect(0.0, 0.0, 220.0, 28.0),
            false,
        );
        for label in ["Reminder", "To-do", "Calendar Event"] {
            kind.addItemWithTitle(&NSString::from_str(label));
        }
        kind.selectItemAtIndex(match editor.kind {
            ActionKind::Reminder => 0,
            ActionKind::Todo => 1,
            ActionKind::CalendarEvent => 2,
        });
        form.addArrangedSubview(&kind);
        form.addArrangedSubview(&stack_label(
            self.mtm(),
            "What should it say?",
            12.0,
            true,
            480.0,
        ));
        let title =
            NSTextField::initWithFrame(NSTextField::alloc(self.mtm()), rect(0.0, 0.0, 480.0, 28.0));
        title.setStringValue(&NSString::from_str(&editor.title));
        form.addArrangedSubview(&title);

        form.addArrangedSubview(&stack_label(self.mtm(), "Date and time", 12.0, true, 480.0));
        let date_picker = NSDatePicker::initWithFrame(
            NSDatePicker::alloc(self.mtm()),
            rect(0.0, 0.0, 480.0, 28.0),
        );
        date_picker.setDatePickerStyle(NSDatePickerStyle::TextFieldAndStepper);
        date_picker.setDatePickerElements(
            NSDatePickerElementFlags::YearMonthDay | NSDatePickerElementFlags::HourMinute,
        );
        let now = NSDate::date();
        date_picker.setMinDate(Some(&now));
        date_picker.setDateValue(&editor.due_at_ms.map_or_else(
            || NSDate::dateWithTimeIntervalSince1970(now.timeIntervalSince1970() + 3_600.0),
            |milliseconds| NSDate::dateWithTimeIntervalSince1970(milliseconds as f64 / 1_000.0),
        ));
        form.addArrangedSubview(&date_picker);
        let no_time = unsafe {
            NSButton::checkboxWithTitle_target_action(
                &NSString::from_str("No date or time (Calendar Events require one)"),
                None,
                None,
                self.mtm(),
            )
        };
        if editor.due_at_ms.is_none() {
            no_time.setState(NSControlStateValueOn);
        }
        form.addArrangedSubview(&no_time);
        form.layoutSubtreeIfNeeded();
        form.setFrameSize(NSSize::new(480.0, form.fittingSize().height.max(60.0)));
        alert.setAccessoryView(Some(&form));
        alert.addButtonWithTitle(&NSString::from_str(&editor.submit_label));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        if alert.runModal() != NSAlertFirstButtonReturn {
            return None;
        }

        let due_at_ms = if no_time.state() == NSControlStateValueOn {
            None
        } else {
            Some((date_picker.dateValue().timeIntervalSince1970() * 1_000.0).round() as i64)
        };
        let kind = match kind.indexOfSelectedItem() {
            1 => ActionKind::Todo,
            2 => ActionKind::CalendarEvent,
            _ => ActionKind::Reminder,
        };
        Some((kind, title.stringValue().to_string(), due_at_ms))
    }

    fn choose(&self, title: &str, explanation: &str, first: &str, second: &str) -> Option<bool> {
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(explanation));
        alert.addButtonWithTitle(&NSString::from_str(first));
        alert.addButtonWithTitle(&NSString::from_str(second));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        let response = alert.runModal();
        if response == NSAlertFirstButtonReturn {
            Some(true)
        } else if response == NSAlertSecondButtonReturn {
            Some(false)
        } else {
            None
        }
    }

    fn choose_integration(&self) -> Option<IntegrationOption> {
        let catalog = ViewModel::integration_catalog();
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&NSString::from_str("Choose a Mac integration"));
        alert.setInformativeText(&NSString::from_str(
            "Native items are reversible. Use Shortcuts for Messages, Home, Maps, Music, and other apps.",
        ));
        let form = vertical_stack(self.mtm(), 8.0, 0.0);
        form.setFrame(rect(0.0, 0.0, 480.0, 90.0));
        let choices = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(self.mtm()),
            rect(0.0, 0.0, 480.0, 28.0),
            false,
        );
        for item in &catalog {
            choices.addItemWithTitle(&NSString::from_str(&format!(
                "{} — {}",
                item.name, item.description
            )));
        }
        form.addArrangedSubview(&choices);
        form.layoutSubtreeIfNeeded();
        form.setFrameSize(NSSize::new(480.0, form.fittingSize().height.max(40.0)));
        alert.setAccessoryView(Some(&form));
        alert.addButtonWithTitle(&NSString::from_str("Continue"));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        if alert.runModal() != NSAlertFirstButtonReturn {
            return None;
        }
        catalog.get(choices.indexOfSelectedItem() as usize).cloned()
    }

    fn prompt_optional(&self, title: &str, explanation: &str) -> Option<Option<String>> {
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(explanation));
        let input =
            NSTextField::initWithFrame(NSTextField::alloc(self.mtm()), rect(0.0, 0.0, 480.0, 28.0));
        alert.setAccessoryView(Some(&input));
        alert.addButtonWithTitle(&NSString::from_str("Save"));
        alert.addButtonWithTitle(&NSString::from_str("No Undo"));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        let response = alert.runModal();
        if response == NSAlertFirstButtonReturn {
            let value = input.stringValue().to_string();
            Some((!value.trim().is_empty()).then(|| value.trim().to_owned()))
        } else if response == NSAlertSecondButtonReturn {
            Some(None)
        } else {
            None
        }
    }

    fn prompt_with_value(&self, title: &str, explanation: &str, value: &str) -> Option<String> {
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(explanation));
        let input =
            NSTextField::initWithFrame(NSTextField::alloc(self.mtm()), rect(0.0, 0.0, 480.0, 28.0));
        input.setStringValue(&NSString::from_str(value));
        alert.setAccessoryView(Some(&input));
        alert.addButtonWithTitle(&NSString::from_str("Save"));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        if alert.runModal() == NSAlertFirstButtonReturn {
            let value = input.stringValue().to_string();
            (!value.trim().is_empty()).then(|| value.trim().to_owned())
        } else {
            None
        }
    }

    fn correct_transcript_flow(&self) {
        let Some(value) = self.prompt(
            "Correct transcript segment",
            "Enter segment number, then |, then the corrected text. Segment numbers are shown beside the transcript.",
        ) else {
            return;
        };
        let Some((ordinal, replacement)) = value.split_once('|') else {
            self.show_error("Use the format: 0|corrected transcript text");
            return;
        };
        let Ok(ordinal) = ordinal.trim().parse::<u32>() else {
            self.show_error("The segment number must be 0 or greater");
            return;
        };
        self.dispatch(Intent::CorrectTranscript {
            ordinal,
            replacement: replacement.trim().into(),
        });
    }

    fn show_error(&self, detail: &str) {
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&NSString::from_str("Captain’s Log could not do that"));
        alert.setInformativeText(&NSString::from_str(detail));
        alert.runModal();
    }

    fn request_permission_flow(&self, permission: Permission) {
        let state = {
            let model = self.ivars().model.borrow();
            match permission {
                Permission::Notifications => model.settings().notifications,
                Permission::Calendar => model.settings().calendar,
                Permission::Reminders => model.settings().reminders,
            }
        };
        if matches!(state, PermissionState::Denied | PermissionState::Restricted) {
            return;
        }
        if state == PermissionState::Allowed {
            return;
        }
        let explanation = match permission {
            Permission::Notifications => {
                "Captain’s Log uses one standard macOS notification for each newly processed recording so you can see what arrived and what actions ran."
            }
            Permission::Calendar => {
                "Captain’s Log uses Calendar access when an automation adds or removes an event for you."
            }
            Permission::Reminders => {
                "Captain’s Log uses Reminders access when an automation adds or removes a reminder or to-do for you."
            }
        };
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&NSString::from_str(&format!(
            "Allow {}?",
            permission.label()
        )));
        alert.setInformativeText(&NSString::from_str(explanation));
        alert.addButtonWithTitle(&NSString::from_str("Continue"));
        alert.addButtonWithTitle(&NSString::from_str("Not now"));
        if alert.runModal() == NSAlertFirstButtonReturn {
            self.dispatch(Intent::RequestPermission(permission));
        }
    }

    fn show_window(&self) {
        let app = NSApplication::sharedApplication(self.mtm());
        app.activate();
        self.ivars().window.makeKeyAndOrderFront(None);
        self.ivars().window.orderFrontRegardless();
    }

    fn poll_app_events(&self) {
        self.sync_native_permissions();
        self.consume_command("open-lake", || {
            self.dispatch(Intent::Navigate(Route::Review));
            self.show_window();
        });
        self.consume_command("request-notification-access", || {
            request_notification_access(self.ivars().controller.state_root().to_owned());
        });
        let calendar_root = self.ivars().controller.state_root().to_owned();
        self.consume_command("request-calendar-access", move || {
            request_calendar_access(calendar_root)
        });
        let reminders_root = self.ivars().controller.state_root().to_owned();
        self.consume_command("request-reminders-access", move || {
            request_reminders_access(reminders_root)
        });

        let events = self
            .ivars()
            .arrivals
            .borrow()
            .try_iter()
            .collect::<Vec<_>>();
        for event in events {
            match event {
                AppEvent::Live(notice) => deliver_native_notification(&notice),
                AppEvent::CatchUp(notices) => {
                    if !notices.is_empty() {
                        *self.ivars().catch_up_mode.borrow_mut() = true;
                        self.dispatch(Intent::Navigate(Route::Review));
                        self.show_window();
                    }
                }
                AppEvent::ModelChanged => {
                    self.dispatch(Intent::Refresh);
                    self.resolve_pending_notification();
                }
            }
        }
    }

    fn sync_native_permissions(&self) {
        let refresh_native_status = {
            let mut ticks = self.ivars().permission_poll_ticks.borrow_mut();
            let refresh = (*ticks).is_multiple_of(20);
            *ticks = ticks.wrapping_add(1);
            refresh
        };
        if refresh_native_status {
            persist_notification_settings(self.ivars().controller.state_root().to_owned());
            persist_eventkit_settings(self.ivars().controller.state_root().to_owned());
        }
        let notification = fs::read(
            self.ivars()
                .controller
                .state_root()
                .join("notifications/status.json"),
        )
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("authorization")
                .and_then(serde_json::Value::as_str)
                .map(permission_state_name)
        })
        .unwrap_or(PermissionState::NotRequested);
        let calendar = eventkit_permission_state(EKEntityType::Event);
        let reminders = eventkit_permission_state(EKEntityType::Reminder);
        let changed = self.ivars().model.borrow_mut().replace_permission_states(
            notification,
            calendar,
            reminders,
        );
        if changed {
            persist_eventkit_settings(self.ivars().controller.state_root().to_owned());
            self.render();
        }
    }

    fn consume_command(&self, name: &str, action: impl FnOnce()) {
        let path = self
            .ivars()
            .controller
            .state_root()
            .join("ui-commands")
            .join(name);
        if path.exists() {
            let _ = fs::remove_file(path);
            action();
        }
    }

    fn handle_notification_response(&self, response: &UNNotificationResponse) -> Result<()> {
        let identifier = response.notification().request().identifier().to_string();
        let recording_id = identifier
            .strip_prefix("recording-")
            .context("notification has no recording identifier")
            .and_then(|value| Uuid::parse_str(value).context("invalid recording identifier"))?;
        let action = response.actionIdentifier().to_string();
        let confirm = action == REVIEW_CONFIRMED;
        *self.ivars().catch_up_mode.borrow_mut() = false;
        *self.ivars().pending_notification.borrow_mut() = Some((recording_id, confirm));
        self.resolve_pending_notification();
        if action == unsafe { UNNotificationDefaultActionIdentifier }.to_string() {
            self.show_window();
        }
        Ok(())
    }

    fn resolve_pending_notification(&self) {
        let pending = *self.ivars().pending_notification.borrow();
        let Some((recording_id, confirm)) = pending else {
            return;
        };
        let available = self.ivars().model.borrow().contains_recording(recording_id);
        if !available {
            return;
        }
        *self.ivars().pending_notification.borrow_mut() = None;
        self.dispatch(Intent::SelectRecording(recording_id));
        if confirm {
            self.dispatch(Intent::ConfirmProcessing);
        }
    }
}

pub fn run(config: &Config, arrivals: Receiver<AppEvent>) -> Result<()> {
    let mtm = MainThreadMarker::new().context("Captain’s Log must run on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    persist_eventkit_settings(config.state_root.clone());

    let controller = AppController::new(config.clone());
    let model = controller.load_model().unwrap_or_else(|error| {
        eprintln!("load initial application view model: {error:#}");
        ViewModel::empty(Settings::default())
    });
    let (window, sidebar, split, list_pane, search, list_document, detail_document) =
        build_window(mtm);
    let delegate = MenuDelegate::new(
        mtm,
        controller,
        arrivals,
        model,
        window,
        sidebar,
        split,
        list_pane,
        search,
        list_document,
        detail_document,
    );
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    add_window_controls(mtm, &delegate);
    delegate.render();
    let menu = build_status_menu(mtm, &delegate);
    let status_item =
        NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
    status_item.setMenu(Some(&menu));
    if let Some(button) = status_item.button(mtm) {
        configure_status_content(mtm, &delegate, &button);
        button.setToolTip(Some(&NSString::from_str("Captain’s Log")));
    }
    *delegate.ivars().status_item.borrow_mut() = Some(status_item.clone());
    delegate.render();

    let center = UNUserNotificationCenter::currentNotificationCenter();
    center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    register_review_category(&center);
    persist_notification_settings(config.state_root.clone());
    let timer = unsafe {
        NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
            0.25,
            &delegate,
            sel!(pollArrivals:),
            None,
            true,
        )
    };
    app.finishLaunching();
    let _lifetime = (delegate, menu, status_item, timer, center);
    app.run();
    Ok(())
}

fn configure_status_content(
    mtm: MainThreadMarker,
    delegate: &Retained<MenuDelegate>,
    button: &objc2_app_kit::NSStatusBarButton,
) {
    button.setTitle(&NSString::new());
    button.setImage(None);

    let content = StatusContentView::new(mtm);
    content.setFrame(rect(0.0, 0.0, 28.0, 24.0));
    content.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );

    if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str("waveform.circle.fill"),
        Some(&NSString::from_str("Captain’s Log")),
    ) {
        image.setTemplate(true);
        let icon = NSImageView::imageViewWithImage(&image, mtm);
        icon.setFrame(rect(3.0, 3.0, 18.0, 18.0));
        content.addSubview(&icon);
        *delegate.ivars().status_icon.borrow_mut() = Some(icon);
    }

    let badge = NSBox::initWithFrame(NSBox::alloc(mtm), rect(20.0, 8.0, 16.0, 15.0));
    badge.setBoxType(NSBoxType::Custom);
    badge.setBorderWidth(0.0);
    badge.setCornerRadius(7.5);
    badge.setFillColor(&NSColor::systemOrangeColor());
    badge.setContentViewMargins(NSSize::new(0.0, 0.0));

    let label = NSTextField::labelWithString(&NSString::new(), mtm);
    label.setFrame(rect(0.0, 0.0, 16.0, 15.0));
    label.setAlignment(NSTextAlignment::Center);
    label.setTextColor(Some(&NSColor::whiteColor()));
    label.setFont(Some(&NSFont::boldSystemFontOfSize(10.0)));
    badge.setContentView(Some(&label));
    badge.setHidden(true);
    content.addSubview(&badge);
    button.addSubview(&content);

    *delegate.ivars().status_content.borrow_mut() = Some(content);
    *delegate.ivars().status_badge.borrow_mut() = Some((badge, label));
}

fn status_badge_width(text: &str) -> f64 {
    match text.chars().count() {
        0 | 1 => 16.0,
        2 => 20.0,
        _ => 27.0,
    }
}

// This tuple is destructured immediately into the delegate's strongly typed retained view slots.
#[allow(clippy::type_complexity)]
fn build_window(
    mtm: MainThreadMarker,
) -> (
    Retained<NSWindow>,
    Retained<NSView>,
    Retained<NSSplitView>,
    Retained<NSView>,
    Retained<NSTextField>,
    Retained<NSView>,
    Retained<NSView>,
) {
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect(0.0, 0.0, 1100.0, 720.0),
            NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Miniaturizable
                | NSWindowStyleMask::Resizable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setTitle(&NSString::from_str("Captain’s Log"));
    window.setMinSize(NSSize::new(900.0, 600.0));
    window.center();
    unsafe { window.setReleasedWhenClosed(false) };
    let content = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 1100.0, 720.0));
    content.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    let split = NSSplitView::initWithFrame(NSSplitView::alloc(mtm), rect(0.0, 0.0, 1100.0, 720.0));
    split.setVertical(true);
    split.setDividerStyle(NSSplitViewDividerStyle::Thin);
    split.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );

    let sidebar = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 170.0, 720.0));
    let list_pane = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 300.0, 720.0));
    let detail_pane = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 630.0, 720.0));
    split.addArrangedSubview(&sidebar);
    split.addArrangedSubview(&list_pane);
    split.addArrangedSubview(&detail_pane);
    split.adjustSubviews();
    split.setPosition_ofDividerAtIndex(170.0, 0);
    split.setPosition_ofDividerAtIndex(470.0, 1);

    let search =
        NSTextField::initWithFrame(NSTextField::alloc(mtm), rect(12.0, 676.0, 276.0, 28.0));
    search.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin,
    );
    search.setPlaceholderString(Some(&NSString::from_str("Search titles and transcripts")));
    search.setHidden(true);
    list_pane.addSubview(&search);

    let list_scroll =
        NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, 300.0, 666.0));
    list_scroll.setHasVerticalScroller(true);
    list_scroll.setDrawsBackground(false);
    list_scroll.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    let list_document = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 300.0, 666.0));
    list_scroll.setDocumentView(Some(&list_document));
    list_pane.addSubview(&list_scroll);

    let detail_scroll =
        NSScrollView::initWithFrame(NSScrollView::alloc(mtm), rect(0.0, 0.0, 630.0, 720.0));
    detail_scroll.setHasVerticalScroller(true);
    detail_scroll.setDrawsBackground(false);
    detail_scroll.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
    let detail_document = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 600.0, 720.0));
    detail_scroll.setDocumentView(Some(&detail_document));
    detail_pane.addSubview(&detail_scroll);

    content.addSubview(&split);
    window.setContentView(Some(&content));
    (
        window,
        sidebar,
        split,
        list_pane,
        search,
        list_document,
        detail_document,
    )
}

fn add_window_controls(mtm: MainThreadMarker, delegate: &MenuDelegate) {
    let sidebar = &delegate.ivars().sidebar;
    add_label(
        sidebar,
        "Audio Lake",
        rect(18.0, 662.0, 134.0, 34.0),
        22.0,
        true,
    );
    for (index, (title, selector)) in [
        ("Review", sel!(showReview:)),
        ("Library", sel!(showLibrary:)),
        ("Automations", sel!(showAutomations:)),
        ("Settings", sel!(openLakeSettings:)),
    ]
    .into_iter()
    .enumerate()
    {
        let button = app_button(mtm, title, selector, delegate);
        button.setFrame(rect(14.0, 610.0 - index as f64 * 42.0, 142.0, 34.0));
        button.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
        sidebar.addSubview(&button);
    }
    let refresh = app_button(mtm, "Refresh", sel!(refreshData:), delegate);
    refresh.setFrame(rect(14.0, 66.0, 68.0, 30.0));
    refresh.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMaxYMargin);
    sidebar.addSubview(&refresh);
    let sync = app_button(mtm, "Sync now", sel!(syncNow:), delegate);
    sync.setFrame(rect(86.0, 66.0, 70.0, 30.0));
    sync.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMaxYMargin);
    sidebar.addSubview(&sync);
    unsafe {
        delegate.ivars().search.setTarget(Some(delegate));
        delegate
            .ivars()
            .search
            .setAction(Some(sel!(searchLibrary:)));
    }
}

fn app_button(
    mtm: MainThreadMarker,
    title: &str,
    selector: objc2::runtime::Sel,
    delegate: &MenuDelegate,
) -> Retained<NSButton> {
    let button = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str(title),
            Some(delegate),
            Some(selector),
            mtm,
        )
    };
    button.setTranslatesAutoresizingMaskIntoConstraints(true);
    button
}

fn vertical_stack(mtm: MainThreadMarker, spacing: f64, inset: f64) -> Retained<NSStackView> {
    let stack = NSStackView::initWithFrame(NSStackView::alloc(mtm), rect(0.0, 0.0, 0.0, 0.0));
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setAlignment(NSLayoutAttribute::Leading);
    stack.setDistribution(NSStackViewDistribution::Fill);
    stack.setSpacing(spacing);
    stack.setEdgeInsets(NSEdgeInsets {
        top: inset,
        left: inset,
        bottom: inset,
        right: inset,
    });
    stack
}

fn horizontal_stack(
    mtm: MainThreadMarker,
    spacing: f64,
    alignment: NSLayoutAttribute,
) -> Retained<NSStackView> {
    let stack = NSStackView::initWithFrame(NSStackView::alloc(mtm), rect(0.0, 0.0, 0.0, 0.0));
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    stack.setAlignment(alignment);
    stack.setDistribution(NSStackViewDistribution::Fill);
    stack.setSpacing(spacing);
    stack
}

fn stack_label(
    mtm: MainThreadMarker,
    text: &str,
    font_size: f64,
    bold: bool,
    available_width: f64,
) -> Retained<NSTextField> {
    let label = NSTextField::wrappingLabelWithString(&NSString::from_str(text), mtm);
    label.setPreferredMaxLayoutWidth(available_width.max(1.0));
    let font = if bold {
        NSFont::boldSystemFontOfSize(font_size)
    } else {
        NSFont::systemFontOfSize(font_size)
    };
    label.setFont(Some(&font));
    label
}

fn add_receipt_field(
    mtm: MainThreadMarker,
    receipt: &NSStackView,
    label: &str,
    value: &str,
    available_width: f64,
) {
    let field = vertical_stack(mtm, 2.0, 0.0);
    field.addArrangedSubview(&stack_label(mtm, label, 11.0, true, available_width));
    field.addArrangedSubview(&stack_label(mtm, value, 14.0, false, available_width));
    receipt.addArrangedSubview(&field);
}

fn receipt_box(mtm: MainThreadMarker, title: &str, content: &NSView) -> Retained<NSBox> {
    let receipt = NSBox::initWithFrame(NSBox::alloc(mtm), rect(0.0, 0.0, 0.0, 0.0));
    receipt.setTitle(&NSString::from_str(title));
    receipt.setContentViewMargins(NSSize::new(12.0, 12.0));
    receipt.setContentView(Some(content));
    receipt.sizeToFit();
    receipt
}

fn mount_stack(document: &NSView, stack: &NSStackView, viewport: NSSize) {
    stack.setFrame(rect(0.0, 0.0, viewport.width, viewport.height));
    stack.layoutSubtreeIfNeeded();
    let fitting = stack.fittingSize();
    let height = fitting.height.max(viewport.height);
    document.setFrameSize(NSSize::new(viewport.width, height));
    stack.setFrame(rect(
        0.0,
        height - fitting.height,
        viewport.width,
        fitting.height,
    ));
    document.addSubview(stack);
}

fn clear_view(view: &NSView) {
    for subview in view.subviews().iter() {
        subview.removeFromSuperview();
    }
}

fn viewport_size(document: &NSView, minimum_width: f64, minimum_height: f64) -> NSSize {
    let size = document
        .enclosingScrollView()
        .map(|scroll| scroll.contentSize())
        .unwrap_or(document.frame().size);
    NSSize::new(
        size.width.max(minimum_width),
        size.height.max(minimum_height),
    )
}

fn viewport_width(document: &NSView, minimum: f64) -> f64 {
    document
        .enclosingScrollView()
        .map(|scroll| scroll.contentSize().width)
        .unwrap_or(document.frame().size.width)
        .max(minimum)
}

fn scroll_to_top_if_changed(
    document: &NSView,
    content_height: f64,
    key: String,
    previous: &RefCell<Option<String>>,
) {
    let changed = previous.borrow().as_deref() != Some(&key);
    if changed {
        *previous.borrow_mut() = Some(key);
        document.scrollPoint(NSPoint::new(0.0, content_height));
    }
}

fn add_label(container: &NSView, text: &str, frame: NSRect, font_size: f64, bold: bool) {
    let label = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str(text),
            None,
            None,
            container.mtm(),
        )
    };
    label.setBordered(false);
    label.setAlignment(NSTextAlignment::Left);
    if let Some(cell) = label.cell() {
        cell.setScrollable(false);
        cell.setWraps(true);
        cell.setUsesSingleLineMode(false);
    }
    label.setFrame(frame);
    let font = if bold {
        NSFont::boldSystemFontOfSize(font_size)
    } else {
        NSFont::systemFontOfSize(font_size)
    };
    label.setFont(Some(&font));
    container.addSubview(&label);
}

fn parse_automation(value: &str) -> Result<Automation, String> {
    let parts = value.split('|').map(str::trim).collect::<Vec<_>>();
    if parts.len() != 5 || parts.iter().any(|part| part.is_empty()) {
        return Err("Use: id|name|mode|trigger|destination".into());
    }
    let mode = match parts[2].to_ascii_lowercase().as_str() {
        "explicit" => RoutingMode::Explicit,
        "hybrid" => RoutingMode::Hybrid,
        "implicit" => RoutingMode::Implicit,
        _ => return Err("Mode must be explicit, hybrid, or implicit".into()),
    };
    Ok(Automation {
        id: parts[0].into(),
        name: parts[1].into(),
        mode,
        trigger: parts[3].into(),
        destination: parts[4].into(),
        enabled: true,
    })
}

fn build_status_menu(mtm: MainThreadMarker, delegate: &MenuDelegate) -> Retained<NSMenu> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Captain’s Log"));
    let open = menu_item(mtm, "Open Captain’s Log", sel!(openLake:), delegate);
    menu.addItem(&open);
    *delegate.ivars().open_menu_item.borrow_mut() = Some(open);
    let source_status = informational_menu_item(mtm, "Recording Source");
    menu.addItem(&source_status);
    *delegate.ivars().source_status_menu_item.borrow_mut() = Some(source_status);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    let sync = menu_item(mtm, "Sync Now", sel!(syncNow:), delegate);
    menu.addItem(&sync);
    *delegate.ivars().sync_menu_item.borrow_mut() = Some(sync);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu.addItem(&menu_item(
        mtm,
        "Settings…",
        sel!(openLakeSettings:),
        delegate,
    ));
    menu
}

fn menu_item(
    mtm: MainThreadMarker,
    title: &str,
    action: objc2::runtime::Sel,
    target: &MenuDelegate,
) -> Retained<NSMenuItem> {
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::new(),
        )
    };
    unsafe { item.setTarget(Some(target)) };
    item
}

fn informational_menu_item(mtm: MainThreadMarker, title: &str) -> Retained<NSMenuItem> {
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            None,
            &NSString::new(),
        )
    };
    item.setEnabled(false);
    item
}

pub(crate) fn unseen_arrivals(state_root: &Path) -> Vec<ArrivalNotice> {
    let events = fs::read_to_string(state_root.join("status/events.ndjson")).unwrap_or_default();
    unseen_arrivals_from_events(&events)
}

fn unseen_arrivals_from_events(events: &str) -> Vec<ArrivalNotice> {
    let mut delivered = HashMap::<Uuid, (i64, ArrivalNotice)>::new();
    let mut reviewed = HashMap::<Uuid, i64>::new();
    for line in events.lines() {
        let Ok(wrapper) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(value) = wrapper.get("event") else {
            continue;
        };
        let Ok(envelope) = serde_json::from_value::<Envelope>(value.clone()) else {
            continue;
        };
        match &envelope.message {
            Message::AudioIngested(audio) => {
                delivered.entry(audio.recording_id).or_insert_with(|| {
                    (
                        envelope.emitted_at_ms,
                        ArrivalNotice {
                            recording_id: audio.recording_id,
                            summary: "A recording is ready for private processing.".into(),
                            hooks: "Processing is not complete yet".into(),
                        },
                    )
                });
            }
            Message::TranscriptCompleted(transcript) => {
                let summary = transcript
                    .segments
                    .iter()
                    .map(|segment| segment.text.trim())
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(180)
                    .collect::<String>();
                delivered
                    .entry(transcript.recording_id)
                    .and_modify(|entry| {
                        entry.0 = envelope.emitted_at_ms;
                        entry.1.summary.clone_from(&summary);
                    })
                    .or_insert_with(|| {
                        (
                            envelope.emitted_at_ms,
                            ArrivalNotice {
                                recording_id: transcript.recording_id,
                                summary,
                                hooks: "Processing is not complete yet".into(),
                            },
                        )
                    });
            }
            Message::NotificationDelivered(delivery) => {
                let entry = delivered
                    .entry(delivery.recording_id)
                    .or_insert_with(|| (envelope.emitted_at_ms, ArrivalNotice::from(delivery)));
                if envelope.emitted_at_ms >= entry.0 {
                    *entry = (envelope.emitted_at_ms, ArrivalNotice::from(delivery));
                }
            }
            Message::RecordingReviewed(review) => {
                reviewed
                    .entry(review.recording_id)
                    .and_modify(|time| *time = (*time).max(envelope.emitted_at_ms))
                    .or_insert(envelope.emitted_at_ms);
            }
            _ => {}
        }
    }
    let mut values = delivered
        .into_iter()
        .filter(|(recording_id, (delivered_at, _))| {
            reviewed
                .get(recording_id)
                .is_none_or(|reviewed_at| reviewed_at < delivered_at)
        })
        .map(|(_, value)| value)
        .collect::<Vec<_>>();
    values.sort_by_key(|(delivered_at, _)| *delivered_at);
    values.into_iter().map(|(_, notice)| notice).collect()
}

fn deliver_native_notification(notice: &ArrivalNotice) {
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str("Captain’s Log received a recording"));
    content.setBody(&NSString::from_str(&format!(
        "{}\n{}",
        notice.summary, notice.hooks
    )));
    content.setThreadIdentifier(&NSString::from_str("personal-audio-lake-arrivals"));
    content.setCategoryIdentifier(&NSString::from_str(REVIEW_CATEGORY));
    content.setSound(Some(&UNNotificationSound::defaultSound()));
    content.setInterruptionLevel(UNNotificationInterruptionLevel::Active);
    content.setRelevanceScore(1.0);
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(&format!("recording-{}", notice.recording_id)),
        &content,
        None,
    );
    UNUserNotificationCenter::currentNotificationCenter()
        .addNotificationRequest_withCompletionHandler(&request, None);
}

fn register_review_category(center: &UNUserNotificationCenter) {
    let confirmed = UNNotificationAction::actionWithIdentifier_title_options(
        &NSString::from_str(REVIEW_CONFIRMED),
        &NSString::from_str("Confirm processing"),
        UNNotificationActionOptionNone,
    );
    let actions = NSArray::from_retained_slice(&[confirmed]);
    let category = UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
        &NSString::from_str(REVIEW_CATEGORY),
        &actions,
        &NSArray::<NSString>::from_slice(&[]),
        UNNotificationCategoryOptionNone,
    );
    center.setNotificationCategories(&NSSet::from_retained_slice(&[category]));
}

fn request_notification_access(state_root: PathBuf) {
    let completion: RcBlock<dyn Fn(Bool, *mut NSError)> =
        RcBlock::new(move |_granted: Bool, _error: *mut NSError| {
            persist_notification_settings(state_root.clone());
        });
    UNUserNotificationCenter::currentNotificationCenter()
        .requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert
                | UNAuthorizationOptions::Sound
                | UNAuthorizationOptions::Badge,
            &completion,
        );
}

fn request_calendar_access(state_root: PathBuf) {
    unsafe {
        let store = EKEventStore::new();
        let completion: RcBlock<dyn Fn(Bool, *mut NSError)> =
            RcBlock::new(move |_granted: Bool, _error: *mut NSError| {
                persist_eventkit_settings(state_root.clone());
            });
        store.requestFullAccessToEventsWithCompletion(&*completion as *const _ as *mut _);
    }
}

fn request_reminders_access(state_root: PathBuf) {
    unsafe {
        let store = EKEventStore::new();
        let completion: RcBlock<dyn Fn(Bool, *mut NSError)> =
            RcBlock::new(move |_granted: Bool, _error: *mut NSError| {
                persist_eventkit_settings(state_root.clone());
            });
        store.requestFullAccessToRemindersWithCompletion(&*completion as *const _ as *mut _);
    }
}

fn persist_eventkit_settings(state_root: PathBuf) {
    let value = NativeEventKitSettings {
        calendar: eventkit_authorization_name(EKEntityType::Event),
        reminders: eventkit_authorization_name(EKEntityType::Reminder),
        managed_by: "com.smfmarin.personal-audio-lake",
    };
    let directory = state_root.join("permissions");
    if let Err(error) = fs::create_dir_all(&directory).and_then(|()| {
        let target = directory.join("eventkit.json");
        let temporary = directory.join("eventkit.json.partial");
        fs::write(
            &temporary,
            serde_json::to_vec_pretty(&value).map_err(std::io::Error::other)?,
        )?;
        fs::rename(temporary, target)
    }) {
        eprintln!("persist Calendar and Reminders settings: {error}");
    }
}

fn eventkit_permission_state(entity: EKEntityType) -> PermissionState {
    permission_state_name(eventkit_authorization_name(entity))
}

fn eventkit_authorization_name(entity: EKEntityType) -> &'static str {
    let status = unsafe { EKEventStore::authorizationStatusForEntityType(entity) };
    if status == EKAuthorizationStatus::FullAccess {
        "authorized"
    } else if status == EKAuthorizationStatus::WriteOnly {
        "write_only"
    } else if status == EKAuthorizationStatus::Denied {
        "denied"
    } else if status == EKAuthorizationStatus::Restricted {
        "restricted"
    } else if status == EKAuthorizationStatus::NotDetermined {
        "not_determined"
    } else {
        "unknown"
    }
}

fn permission_state_name(status: &str) -> PermissionState {
    match status {
        "authorized" | "allowed" | "provisional" => PermissionState::Allowed,
        "denied" => PermissionState::Denied,
        "restricted" => PermissionState::Restricted,
        "not_determined" | "not requested" => PermissionState::NotRequested,
        _ => PermissionState::Unknown,
    }
}

fn persist_notification_settings(state_root: PathBuf) {
    let completion: RcBlock<dyn Fn(NonNull<UNNotificationSettings>)> =
        RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            let settings = unsafe { settings.as_ref() };
            let value = NativeNotificationSettings {
                authorization: authorization_name(settings.authorizationStatus()),
                alerts_enabled: settings.alertSetting() == UNNotificationSetting::Enabled,
                sound_enabled: settings.soundSetting() == UNNotificationSetting::Enabled,
                notification_center_enabled: settings.notificationCenterSetting()
                    == UNNotificationSetting::Enabled,
                time_sensitive_enabled: settings.timeSensitiveSetting()
                    == UNNotificationSetting::Enabled,
                alert_style: alert_style_name(settings.alertStyle()),
                managed_by: "com.smfmarin.personal-audio-lake",
            };
            let directory = state_root.join("notifications");
            if let Err(error) = fs::create_dir_all(&directory).and_then(|()| {
                let target = directory.join("status.json");
                let temporary = directory.join("status.json.partial");
                fs::write(
                    &temporary,
                    serde_json::to_vec_pretty(&value).map_err(std::io::Error::other)?,
                )?;
                fs::rename(temporary, target)
            }) {
                eprintln!("persist notification settings: {error}");
            }
        });
    UNUserNotificationCenter::currentNotificationCenter()
        .getNotificationSettingsWithCompletionHandler(&completion);
}

fn authorization_name(status: UNAuthorizationStatus) -> &'static str {
    if status == UNAuthorizationStatus::NotDetermined {
        "not_determined"
    } else if status == UNAuthorizationStatus::Denied {
        "denied"
    } else if status == UNAuthorizationStatus::Authorized {
        "authorized"
    } else if status == UNAuthorizationStatus::Provisional {
        "provisional"
    } else if status == UNAuthorizationStatus::Ephemeral {
        "ephemeral"
    } else {
        "unknown"
    }
}

fn alert_style_name(style: UNAlertStyle) -> &'static str {
    if style == UNAlertStyle::Alert {
        "alert"
    } else if style == UNAlertStyle::Banner {
        "banner"
    } else if style == UNAlertStyle::None {
        "none"
    } else {
        "unknown"
    }
}

fn action_kind(kind: ActionKind) -> &'static str {
    match kind {
        ActionKind::Todo => "Todo",
        ActionKind::Reminder => "Reminder",
        ActionKind::CalendarEvent => "Calendar",
    }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use personal_audio_lake_contracts::{NotificationHook, RecordingReview, ReviewOutcome};

    #[test]
    fn arrival_notice_explicitly_reports_no_hooks() {
        let notice = ArrivalNotice::from(&NotificationDelivery {
            recording_id: Uuid::new_v4(),
            delivered_at_ms: 1,
            summary: "A short note".into(),
            hooks: vec![],
        });
        assert_eq!(notice.hooks, "Nothing was added");
    }

    #[test]
    fn status_badge_reserves_space_for_the_visible_count() {
        assert_eq!(status_badge_width("1"), 16.0);
        assert_eq!(status_badge_width("12"), 20.0);
        assert_eq!(status_badge_width("99+"), 27.0);
    }

    #[test]
    fn arrival_notice_describes_each_hook_deterministically() {
        let notice = ArrivalNotice::from(&NotificationDelivery {
            recording_id: Uuid::new_v4(),
            delivered_at_ms: 1,
            summary: "A reminder".into(),
            hooks: vec![NotificationHook {
                action_id: Uuid::new_v4(),
                kind: ActionKind::Reminder,
                title: "Buy milk".into(),
                status: "awaiting_review".into(),
                target: None,
            }],
        });
        assert_eq!(notice.hooks, "Reminder: Buy milk — Waiting");
    }

    #[test]
    fn catch_up_only_contains_deliveries_newer_than_review() {
        let recording_id = Uuid::new_v4();
        let delivery = Envelope::root(Message::NotificationDelivered(NotificationDelivery {
            recording_id,
            delivered_at_ms: 1,
            summary: "One".into(),
            hooks: vec![],
        }));
        let review = Envelope::root(Message::RecordingReviewed(RecordingReview {
            review_id: Uuid::new_v4(),
            recording_id,
            outcome: ReviewOutcome::Confirmed,
            note: None,
            reviewed_at_ms: 2,
        }));
        let events = format!(
            "{}\n{}\n",
            serde_json::json!({"event":delivery}),
            serde_json::json!({"event":review})
        );
        assert!(unseen_arrivals_from_events(&events).is_empty());
    }

    #[test]
    fn catch_up_includes_transferred_recordings_even_without_a_notification() {
        let audio = Envelope::root(Message::AudioIngested(
            personal_audio_lake_contracts::AudioIngested {
                recording_id: Uuid::new_v4(),
                source_id: "fixture-source".into(),
                sha256: "0".repeat(64),
                relative_path: "raw/audio.mp3".into(),
                byte_len: 12,
                recorded_at_ms: 1,
                ingested_at_ms: 2,
            },
        ));
        let events = format!("{}\n", serde_json::json!({"event":audio}));
        assert_eq!(unseen_arrivals_from_events(&events).len(), 1);
    }

    #[test]
    fn catch_up_is_quiet_when_there_are_no_arrivals() {
        assert!(unseen_arrivals_from_events("").is_empty());
    }

    #[test]
    fn catch_up_keeps_three_unseen_arrivals_in_one_queue() {
        let events = (0..3)
            .map(|index| {
                let audio = Envelope::root(Message::AudioIngested(
                    personal_audio_lake_contracts::AudioIngested {
                        recording_id: Uuid::new_v4(),
                        source_id: "fixture-source".into(),
                        sha256: format!("{index}").repeat(64),
                        relative_path: format!("raw/{index}.mp3"),
                        byte_len: 12,
                        recorded_at_ms: index,
                        ingested_at_ms: index + 1,
                    },
                ));
                format!("{}\n", serde_json::json!({"event":audio}))
            })
            .collect::<String>();
        assert_eq!(unseen_arrivals_from_events(&events).len(), 3);
    }
}
