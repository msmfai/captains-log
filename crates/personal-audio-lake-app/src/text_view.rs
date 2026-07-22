//! A deterministic, disposable text view over the same model used by native UI adapters.

use std::fmt::Write as _;

use personal_audio_lake_contracts::{
    ActionKind, WorkflowAction, WorkflowAdapter, WorkflowDefinition, WorkflowSelection,
    WorkflowSelector, WorkflowState, WorkflowTransition,
};

use crate::view_model::{Automation, Intent, Permission, Route, RoutingMode, ViewModel};

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Intent(Intent),
    Render,
    Help,
    Quit,
}

pub fn parse(input: &str) -> Result<Input, String> {
    let input = input.trim();
    if input.is_empty() || input == "show" {
        return Ok(Input::Render);
    }
    let (command, rest) = input
        .split_once(char::is_whitespace)
        .map_or((input, ""), |(command, rest)| (command, rest.trim()));
    match command.to_ascii_lowercase().as_str() {
        "review" => Ok(navigate(Route::Review)),
        "library" => Ok(navigate(Route::Library)),
        "automations" => Ok(navigate(Route::Automations)),
        "settings" => Ok(navigate(Route::Settings)),
        "search" => Ok(Input::Intent(Intent::Search(rest.into()))),
        "select" => Ok(Input::Intent(Intent::SelectVisible(one_based(
            rest,
            "recording",
        )?))),
        "refresh" => Ok(Input::Intent(Intent::Refresh)),
        "sync" => Ok(Input::Intent(Intent::Sync)),
        "source" => Ok(Input::Intent(Intent::SelectSourceAdapter(one_based(
            rest,
            "recording source",
        )?))),
        "release" | "release-source" | "release-device" => Ok(Input::Intent(Intent::ReleaseSource)),
        "play" => Ok(Input::Intent(Intent::PlayAudio)),
        "reviewed" | "confirm" | "ok" | "right" => Ok(Input::Intent(Intent::ConfirmProcessing)),
        "correct" => {
            let (ordinal, replacement) = rest
                .split_once(char::is_whitespace)
                .ok_or_else(|| "Usage: correct <segment> <replacement text>".to_owned())?;
            Ok(Input::Intent(Intent::CorrectTranscript {
                ordinal: ordinal
                    .parse()
                    .map_err(|_| "Transcript segment must be a non-negative number".to_owned())?,
                replacement: required(
                    replacement.trim(),
                    "Usage: correct <segment> <replacement text>",
                )?,
            }))
        }
        "approve" => Ok(Input::Intent(Intent::ApproveAction(one_based(
            rest, "action",
        )?))),
        "reject" => {
            let (index, reason) = rest
                .split_once(char::is_whitespace)
                .ok_or_else(|| "Usage: reject <action> <reason>".to_owned())?;
            Ok(Input::Intent(Intent::RejectAction {
                index: one_based(index, "action")?,
                reason: required(reason.trim(), "A rejection reason is required")?,
            }))
        }
        "remove" | "undo-action" => Ok(Input::Intent(Intent::RemoveAction(one_based(
            rest, "action",
        )?))),
        "edit-action" => {
            let (index, values) = rest.split_once(char::is_whitespace).ok_or_else(|| {
                "Usage: edit-action <action> <reminder|todo|calendar>|<title>|<due-at-ms-or-empty>"
                    .to_owned()
            })?;
            let mut values = values.splitn(3, '|');
            let kind = match values.next().unwrap_or_default().trim() {
                "reminder" => ActionKind::Reminder,
                "todo" => ActionKind::Todo,
                "calendar" | "calendar_event" => ActionKind::CalendarEvent,
                _ => return Err("Action type must be reminder, todo, or calendar".into()),
            };
            let title = values.next().unwrap_or_default();
            let due_at_ms = values.next().ok_or_else(|| {
                "Usage: edit-action <action> <reminder|todo|calendar>|<title>|<due-at-ms-or-empty>"
                    .to_owned()
            })?;
            let due_at_ms = (!due_at_ms.trim().is_empty())
                .then(|| {
                    due_at_ms
                        .trim()
                        .parse::<i64>()
                        .map_err(|_| "Action time must be Unix milliseconds".to_owned())
                })
                .transpose()?;
            Ok(Input::Intent(Intent::EditAction {
                index: one_based(index, "action")?,
                kind,
                title: required(title.trim(), "Action text is required")?,
                due_at_ms,
            }))
        }
        "workflow-run" => {
            let mut parts = rest.splitn(3, char::is_whitespace);
            let workflow = parts.next().unwrap_or_default();
            let transition_id = parts.next().unwrap_or_default();
            let outputs = parts.next().unwrap_or_default();
            if transition_id.is_empty() {
                return Err(
                    "Usage: workflow-run <workflow> <transition> [field=value;field=value]".into(),
                );
            }
            Ok(Input::Intent(Intent::RunWorkflow {
                index: one_based(workflow, "workflow")?,
                selection: WorkflowSelection {
                    transition_id: transition_id.into(),
                    outputs: parse_outputs(outputs)?,
                },
            }))
        }
        "workflow-route" => Ok(Input::Intent(Intent::RouteWithLocalModel)),
        "workflow-adjust" => {
            let mut parts = rest.splitn(4, char::is_whitespace);
            let run = parts.next().unwrap_or_default();
            let state_id = parts.next().unwrap_or_default();
            let transition_id = parts.next().unwrap_or_default();
            let outputs = parts.next().unwrap_or_default();
            if state_id.is_empty() || transition_id.is_empty() {
                return Err(
                    "Usage: workflow-adjust <run> <state> <transition> [field=value;field=value]"
                        .into(),
                );
            }
            Ok(Input::Intent(Intent::AdjustWorkflowRun {
                run_index: one_based(run, "run")?,
                state_id: state_id.into(),
                transition_id: transition_id.into(),
                outputs: parse_outputs(outputs)?,
            }))
        }
        "workflow-undo" => {
            let (run, step) = rest
                .split_once(char::is_whitespace)
                .ok_or_else(|| "Usage: workflow-undo <run> <step>".to_owned())?;
            Ok(Input::Intent(Intent::UndoWorkflowStep {
                run_index: one_based(run, "run")?,
                step_index: one_based(step, "step")?,
            }))
        }
        "toggle" => Ok(Input::Intent(Intent::ToggleAutomation(one_based(
            rest,
            "automation",
        )?))),
        "create" => Ok(Input::Intent(Intent::CreateAutomation(parse_automation(
            rest,
        )?))),
        "workflow-toggle" => Ok(Input::Intent(Intent::ToggleWorkflow(one_based(
            rest, "workflow",
        )?))),
        "workflow-create" => Ok(Input::Intent(Intent::CreateWorkflow(parse_workflow(rest)?))),
        "workflow-delete" => Ok(Input::Intent(Intent::DeleteWorkflow(one_based(
            rest, "workflow",
        )?))),
        "allow" => Ok(Input::Intent(Intent::RequestPermission(parse_permission(
            rest,
        )?))),
        "help" | "?" => Ok(Input::Help),
        "quit" | "exit" => Ok(Input::Quit),
        _ => Err(format!("Unknown command '{command}'. Type help.")),
    }
}

fn parse_outputs(input: &str) -> Result<std::collections::BTreeMap<String, String>, String> {
    input
        .split(';')
        .filter(|value| !value.trim().is_empty())
        .map(|pair| {
            let (field, value) = pair
                .split_once('=')
                .ok_or_else(|| format!("Output '{pair}' must use field=value"))?;
            let field = field.trim();
            if field.is_empty() {
                return Err("An output field name cannot be empty".into());
            }
            Ok((field.to_owned(), value.trim().to_owned()))
        })
        .collect()
}

fn navigate(route: Route) -> Input {
    Input::Intent(Intent::Navigate(route))
}

fn one_based(value: &str, noun: &str) -> Result<usize, String> {
    let parsed = value
        .trim()
        .parse::<usize>()
        .map_err(|_| format!("{noun} must be a number shown by the view"))?;
    parsed
        .checked_sub(1)
        .ok_or_else(|| format!("{noun} numbers start at 1"))
}

fn required(value: &str, error: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        Err(error.into())
    } else {
        Ok(value.trim().into())
    }
}

fn parse_permission(value: &str) -> Result<Permission, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "notifications" | "notification" => Ok(Permission::Notifications),
        "calendar" => Ok(Permission::Calendar),
        "reminders" | "reminder" => Ok(Permission::Reminders),
        _ => Err("Usage: allow notifications|calendar|reminders".into()),
    }
}

fn parse_automation(value: &str) -> Result<Automation, String> {
    let parts = value.split('|').map(str::trim).collect::<Vec<_>>();
    if parts.len() != 5 || parts.iter().any(|part| part.is_empty()) {
        return Err(
            "Usage: create <id>|<name>|<mode>|<trigger>|<destination> (mode: explicit, hybrid, or implicit)".into(),
        );
    }
    let mode = match parts[2].to_ascii_lowercase().as_str() {
        "explicit" => RoutingMode::Explicit,
        "hybrid" => RoutingMode::Hybrid,
        "implicit" => RoutingMode::Implicit,
        _ => return Err("Automation mode must be explicit, hybrid, or implicit".into()),
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

fn parse_workflow(value: &str) -> Result<WorkflowDefinition, String> {
    if value.trim_start().starts_with('{') {
        return serde_json::from_str(value)
            .map_err(|error| format!("Invalid workflow JSON: {error}"));
    }
    let parts = value.split('|').map(str::trim).collect::<Vec<_>>();
    if parts.len() != 6 || parts[..5].iter().any(|part| part.is_empty()) {
        return Err(
            "Usage: workflow-create <id>|<name>|<description>|<shortcut|webhook>|<target>|<undo-target-or-empty>".into(),
        );
    }
    let make_adapter = |target: &str| match parts[3].to_ascii_lowercase().as_str() {
        "shortcut" => Ok(WorkflowAdapter::Shortcut {
            target: target.into(),
        }),
        "webhook" => Ok(WorkflowAdapter::Webhook {
            target: target.into(),
        }),
        _ => Err("Workflow adapter must be shortcut or webhook".to_owned()),
    };
    let undo_adapter = (!parts[5].is_empty())
        .then(|| make_adapter(parts[5]))
        .transpose()?;
    Ok(WorkflowDefinition {
        id: parts[0].into(),
        name: parts[1].into(),
        description: parts[2].into(),
        enabled: true,
        subscriptions: vec!["transcript_completed".into()],
        initial_state_id: "message_ingested".into(),
        states: vec![
            WorkflowState {
                id: "message_ingested".into(),
                description: "A new voice message and transcript have been ingested".into(),
                selector: WorkflowSelector::Deterministic {
                    transition_id: "run".into(),
                },
                transitions: vec![WorkflowTransition {
                    id: "run".into(),
                    description: "Run the configured effect".into(),
                    destination_state_id: "done".into(),
                    output_fields: vec![],
                    action: WorkflowAction::Effect {
                        adapter: make_adapter(parts[4])?,
                        undo_adapter,
                    },
                }],
            },
            WorkflowState {
                id: "done".into(),
                description: "Workflow completed".into(),
                selector: WorkflowSelector::Terminal {
                    outcome: "complete".into(),
                },
                transitions: vec![],
            },
        ],
    })
}

pub fn render(model: &ViewModel) -> String {
    let mut output = String::new();
    writeln!(output, "Captain’s Log / {}", model.route().label()).unwrap();
    for route in Route::ALL {
        write!(
            output,
            "{}{}{}",
            if route == model.route() { "[" } else { " " },
            route.label(),
            if route == model.route() { "] " } else { "  " }
        )
        .unwrap();
    }
    output.push_str("\n\n");
    if let Some(feedback) = model.feedback() {
        writeln!(output, "✓ {feedback}\n").unwrap();
    }
    match model.route() {
        Route::Review => render_recordings(model, &mut output, true),
        Route::Library => render_recordings(model, &mut output, false),
        Route::Automations => render_automations(model, &mut output),
        Route::Settings => render_settings(model, &mut output),
    }
    let failures = model.failures().collect::<Vec<_>>();
    if !failures.is_empty() {
        output.push_str("\nNeeds attention\n");
        for failure in failures {
            writeln!(output, "! {failure}").unwrap();
        }
    }
    output
}

fn render_recordings(model: &ViewModel, output: &mut String, review: bool) {
    let rows = model.library_rows();
    if review {
        writeln!(
            output,
            "{} recording{} waiting for review",
            rows.len(),
            if rows.len() == 1 { "" } else { "s" }
        )
        .unwrap();
        output.push_str("Recordings stay here until you say everything looks right. Actions have already run; correct or undo anything the app got wrong.\n");
    } else {
        writeln!(
            output,
            "Search: {}",
            if model.library_query().is_empty() {
                "(all recordings)"
            } else {
                model.library_query()
            }
        )
        .unwrap();
    }
    if rows.is_empty() {
        output.push_str(if review {
            "You’re all caught up.\n"
        } else {
            "No recordings match.\n"
        });
        output.push_str("\nCommands: refresh, sync, library, automations, settings, help\n");
        return;
    }
    output.push('\n');
    for (index, row) in rows.iter().enumerate() {
        writeln!(
            output,
            "{}{}. {} — {}",
            if row.selected { "> " } else { "  " },
            index + 1,
            row.subtitle,
            row.title,
        )
        .unwrap();
    }
    let Some(detail) = model.recording_presentation() else {
        return;
    };
    writeln!(output, "\nSelected: {}", detail.header.title).unwrap();
    writeln!(output, "Received: {}", detail.header.metadata).unwrap();
    writeln!(
        output,
        "Audio: {}",
        if detail.header.audio_available {
            "Available (command: play)"
        } else {
            "Waiting for transfer"
        }
    )
    .unwrap();
    writeln!(output, "Review: {}", detail.header.review_status).unwrap();
    if let Some(review) = &detail.review_acknowledgement {
        writeln!(output, "{}\n{}", review.heading, review.explanation).unwrap();
        writeln!(
            output,
            "Accept: {} (command: reviewed)",
            review.action_label
        )
        .unwrap();
    }
    writeln!(output, "\nSummary\n{}", detail.summary).unwrap();
    output.push_str("\nTranscript\n");
    for segment in &detail.transcript {
        writeln!(
            output,
            "[{} @ {}] {}{}",
            segment.ordinal,
            segment.time_label,
            segment.text,
            if segment.corrected {
                "  (corrected overlay)"
            } else {
                ""
            }
        )
        .unwrap();
    }
    output.push_str("\nAutomation History\n");
    if detail.activity.is_empty() {
        output.push_str("No automations ran for this recording.\n");
    }
    for receipt in &detail.activity {
        writeln!(
            output,
            "{}. Automation: {}\n   Started: {}\n   Status: {}\n   Outcome: {}\n   Checked: {}",
            receipt.run_index + 1,
            receipt.workflow,
            receipt.triggered_by,
            receipt.state,
            receipt.result,
            receipt.confirmation,
        )
        .unwrap();
    }
    if !detail.technical_trace.is_empty() {
        writeln!(output, "\nDetails\n{}", detail.technical_trace).unwrap();
    }
    output.push_str("\nActions Taken\n");
    if let Some(message) = detail.no_action_message {
        writeln!(output, "{message}").unwrap();
    }
    for action in &detail.actions {
        writeln!(
            output,
            "{}. Action: {}\n   {}: {}\n   Destination: {}\n   Status: {}\n   Result: {}",
            action.index + 1,
            action.operation,
            action.payload_label,
            action.payload,
            action.destination,
            action.state_label,
            action.state_detail,
        )
        .unwrap();
        if let Some(evidence) = &action.evidence_quote {
            writeln!(output, "   Why this action was chosen: {evidence}").unwrap();
        }
        if let Some(scheduled_for) = &action.scheduled_for {
            writeln!(output, "   When: {scheduled_for}").unwrap();
        }
        writeln!(
            output,
            "   Available: {} (command: edit-action {} TYPE|TITLE|DUE_MS)",
            action.edit_label,
            action.index + 1
        )
        .unwrap();
        if let Some(label) = &action.remove_label {
            writeln!(
                output,
                "   Available: {label} (command: remove {})",
                action.index + 1
            )
            .unwrap();
        }
    }
    output.push_str(
        "\nCommands: select N, play, correct SEGMENT TEXT, edit-action ACTION TYPE|TITLE|DUE_MS, remove ACTION, workflow-run N TRANSITION [field=value;...], workflow-route, workflow-adjust RUN STATE TRANSITION [field=value;...], workflow-undo RUN STEP, reviewed, refresh, sync, library, automations, settings, help\n",
    );
}

fn render_automations(model: &ViewModel, output: &mut String) {
    output.push_str("Automations\n");
    output.push_str("The app checks every new recording and can add reminders, to-dos, or calendar events automatically.\n\n");
    let workflows = model.workflow_rows();
    if workflows.is_empty() {
        output.push_str("The app is still loading your automations.\n");
    }
    for workflow in workflows {
        writeln!(
            output,
            "{}. {} — {}\n   {}",
            workflow.index + 1,
            workflow.name,
            workflow.status_label,
            workflow.description,
        )
        .unwrap();
    }
    output.push_str(
        "\nCommands: workflow-toggle N, workflow-create ID|NAME|DESCRIPTION|ADAPTER|TARGET|UNDO-TARGET-OR-EMPTY, workflow-delete N, review, library, settings, help\n",
    );
}

fn render_settings(model: &ViewModel, output: &mut String) {
    let settings = model.settings_presentation();
    writeln!(
        output,
        "Recorder App Transfer\n{}\n{}\n{}",
        settings.recording_transfer_status,
        settings.recording_transfer_detail,
        settings.recording_transfer_help
    )
    .unwrap();
    writeln!(
        output,
        "\nCompatibility Device\n{} — {}\nOpen at Login: {}",
        settings.source_name, settings.connection, settings.open_at_login
    )
    .unwrap();
    for (index, option) in settings.source_options.iter().enumerate() {
        writeln!(
            output,
            "{}. {}{}\n   {}",
            index + 1,
            option.name,
            if option.selected { " — Selected" } else { "" },
            option.description
        )
        .unwrap();
    }
    writeln!(
        output,
        "\nPrivacy & Storage\nProcessing: {}\nStorage Location: {}",
        settings.processing, settings.storage
    )
    .unwrap();
    output.push_str("\nMac Permissions\n");
    for permission in settings.permissions {
        writeln!(output, "{}: {}", permission.name, permission.status).unwrap();
    }
    output.push_str(
        "\nCommands: source N, allow notifications|calendar|reminders, sync, release-device, review, library, automations, help\n",
    );
}

pub fn help() -> &'static str {
    "Navigation: review | library | automations | settings\n\
Recordings: select N | search TEXT | play | correct SEGMENT TEXT | edit-action ACTION TYPE|TITLE|DUE_MS | reviewed\n\
Runs: workflow-run N TRANSITION [field=value;...] | workflow-route | workflow-adjust RUN STATE TRANSITION [field=value;...] | workflow-undo RUN STEP\n\
Workflows: workflow-toggle N | workflow-create ID|NAME|DESCRIPTION|ADAPTER|TARGET|UNDO-TARGET | workflow-delete N\n\
System: refresh | source N | sync | release-device | allow notifications|calendar|reminders | show | quit"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        read_model::fixture_snapshot,
        view_model::{Settings, ViewModel},
    };

    fn model() -> ViewModel {
        ViewModel::new(
            fixture_snapshot(),
            Vec::new(),
            Vec::new(),
            Settings::default(),
        )
    }

    #[test]
    fn text_view_exposes_the_four_routes_and_action_receipt() {
        let rendered = render(&model());
        assert!(rendered.contains("[Review]"));
        assert!(rendered.contains("Library"));
        assert!(rendered.contains("Automations"));
        assert!(rendered.contains("Settings"));
        assert!(rendered.contains("Action: Create a reminder"));
        assert!(rendered.contains("Destination: Apple Reminders"));
        assert!(rendered.contains("Status: Completed"));
        assert!(rendered.contains("Result: A matching item was created in Apple Reminders."));
    }

    #[test]
    fn commands_are_parsed_to_renderer_independent_intents() {
        assert_eq!(
            parse("reviewed"),
            Ok(Input::Intent(Intent::ConfirmProcessing))
        );
        assert_eq!(
            parse("correct 3 actually next Tuesday"),
            Ok(Input::Intent(Intent::CorrectTranscript {
                ordinal: 3,
                replacement: "actually next Tuesday".into(),
            }))
        );
        assert_eq!(
            parse("workflow-route"),
            Ok(Input::Intent(Intent::RouteWithLocalModel))
        );
        assert_eq!(
            parse("source 1"),
            Ok(Input::Intent(Intent::SelectSourceAdapter(0)))
        );
        assert_eq!(
            parse("edit-action 1 reminder|Call the dentist|1800000000000"),
            Ok(Input::Intent(Intent::EditAction {
                index: 0,
                kind: ActionKind::Reminder,
                title: "Call the dentist".into(),
                due_at_ms: Some(1_800_000_000_000),
            }))
        );
        assert_eq!(
            parse("workflow-run 2 create title=Buy milk;due_at_ms=1784044800000"),
            Ok(Input::Intent(Intent::RunWorkflow {
                index: 1,
                selection: WorkflowSelection {
                    transition_id: "create".into(),
                    outputs: [
                        ("due_at_ms".into(), "1784044800000".into()),
                        ("title".into(), "Buy milk".into()),
                    ]
                    .into(),
                },
            }))
        );
    }

    #[test]
    fn invalid_indexes_never_silently_target_an_item() {
        assert!(parse("select 0").is_err());
        assert!(parse("approve nope").is_err());
    }
}
