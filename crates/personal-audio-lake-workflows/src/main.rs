use std::io::{self, BufRead, Write};

use anyhow::{Context, Result, bail};
use chrono::{Datelike, Duration, LocalResult, TimeZone, Utc};
use chrono_tz::Tz;
use personal_audio_lake_contracts::{
    ActionKind, ActionProposal, DerivationCompleted, DerivationRequest, DerivedAction, Envelope,
    Failure, HealthState, Message, SegmentEvidence, TranscriptCompleted, WorkerHealth, WorkerRole,
    is_required_local_model_size,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const EXTRACTOR_VERSION: &str = "configurable-rules-2";
const ACTION_NAMESPACE: Uuid = Uuid::from_u128(0xfd20c83c_5535_429c_887f_7b243dc8df16);

struct Extractor {
    time: Regex,
    timed_reminder: Regex,
    rules: Vec<Rule>,
    semantic_model: String,
    ollama_url: String,
    http: reqwest::blocking::Client,
}

#[derive(Debug, Clone, Deserialize)]
struct RuleSet {
    #[serde(default = "default_semantic_model")]
    semantic_model: String,
    #[serde(default = "default_ollama_url")]
    ollama_url: String,
    rules: Vec<Rule>,
}

#[derive(Debug, Clone, Deserialize)]
struct Rule {
    id: String,
    #[serde(default = "yes")]
    enabled: bool,
    mode: RuleMode,
    #[serde(default)]
    phrases: Vec<String>,
    #[serde(default = "default_threshold")]
    threshold: f32,
    action: String,
    #[serde(default = "review_policy")]
    approval: String,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RuleMode {
    Explicit,
    Hybrid,
    Implicit,
}

#[derive(Debug, Clone, Serialize)]
struct ModelCandidate {
    rule_id: String,
    mode: String,
    allowed_action: String,
    trigger_confidence: f32,
    ordinal: u32,
    start_ms: u64,
    end_ms: u64,
    transcript_text: String,
    trigger_payload: String,
}

#[derive(Debug, Deserialize)]
struct JudgedActions {
    actions: Vec<Judgment>,
}

#[derive(Debug, Deserialize)]
struct Judgment {
    rule_id: String,
    evidence_ordinal: u32,
    kind: ActionKind,
    title: String,
    due_at_ms: Option<i64>,
    original_time_phrase: Option<String>,
    confidence: f32,
}

fn yes() -> bool {
    true
}
fn default_threshold() -> f32 {
    0.78
}
fn review_policy() -> String {
    "always_review".into()
}
fn default_semantic_model() -> String {
    "mistral-small3.2:latest".into()
}
fn default_ollama_url() -> String {
    "http://127.0.0.1:11434".into()
}

impl Extractor {
    fn new() -> Self {
        Self {
            time: Regex::new(
                r"(?i)\b(today|tomorrow|next (?:monday|tuesday|wednesday|thursday|friday|saturday|sunday))(?:\s+at\s+([0-9]{1,2})(?::([0-9]{2}))?\s*(am|pm)?)?\b",
            )
            .expect("valid time regex"),
            timed_reminder: Regex::new(
                r"(?i)\b(?:remind(?:er)?\s+me|remember)\s+(?:today|tomorrow|next (?:monday|tuesday|wednesday|thursday|friday|saturday|sunday))(?:\s+at\s+[0-9]{1,2}(?::[0-9]{2})?\s*(?:am|pm)?)?\s+to\s+(?<payload>.+)",
            )
            .expect("valid timed reminder regex"),
            rules: default_rules(),
            semantic_model: default_semantic_model(),
            ollama_url: default_ollama_url(),
            http: reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(std::time::Duration::from_secs(600))
                .build()
                .expect("local HTTP client"),
        }
    }

    fn configured() -> Self {
        let mut extractor = Self::new();
        let Some(root) = std::env::var_os("PERSONAL_AUDIO_LAKE_STATE_ROOT") else {
            return extractor;
        };
        let path = std::path::PathBuf::from(root).join("rules.json");
        if let Ok(bytes) = std::fs::read(path)
            && let Ok(parsed) = serde_json::from_slice::<RuleSet>(&bytes)
        {
            extractor.rules = parsed.rules;
            extractor.semantic_model = parsed.semantic_model;
            extractor.ollama_url = parsed.ollama_url;
        }
        extractor
    }

    fn derive(&self, request: &DerivationRequest) -> Vec<ActionProposal> {
        let mut results = Vec::new();
        for segment in &request.segments {
            for rule in self.rules.iter().filter(|rule| rule.enabled) {
                let Some((raw_title, trigger_confidence)) = match_rule(rule, &segment.text) else {
                    continue;
                };
                let time_capture = self.time.captures(raw_title);
                let (due_at_ms, phrase) = time_capture
                    .as_ref()
                    .map(|capture| {
                        let phrase = capture.get(0).map(|value| value.as_str().to_owned());
                        (
                            self.resolve_time(capture, request.recorded_at_ms, &request.timezone),
                            phrase,
                        )
                    })
                    .unwrap_or((None, None));
                let title = if let Some(time_match) = time_capture.and_then(|value| value.get(0)) {
                    raw_title
                        .replace(time_match.as_str(), "")
                        .trim_matches([' ', ',', '.'])
                        .to_owned()
                } else {
                    raw_title.trim_matches([' ', ',', '.']).to_owned()
                };
                if title.is_empty() {
                    continue;
                }
                let kind = match rule.action.as_str() {
                    "macos.reminder" => ActionKind::Reminder,
                    "macos.calendar" => ActionKind::CalendarEvent,
                    _ => ActionKind::Todo,
                };
                let identity = format!(
                    "{}:{}:{}:{}:{title}:{due_at_ms:?}",
                    request.recording_id, request.transcript_run_id, segment.ordinal, rule.id
                );
                let time_confidence = if kind == ActionKind::CalendarEvent && due_at_ms.is_none() {
                    0.7
                } else {
                    1.0
                };
                results.push(ActionProposal {
                    action_id: Uuid::new_v5(&ACTION_NAMESPACE, identity.as_bytes()),
                    recording_id: request.recording_id,
                    transcript_run_id: request.transcript_run_id,
                    kind,
                    title,
                    due_at_ms,
                    original_time_phrase: phrase,
                    timezone: request.timezone.clone(),
                    confidence: trigger_confidence * time_confidence,
                    evidence: vec![SegmentEvidence {
                        ordinal: segment.ordinal,
                        start_ms: segment.start_ms,
                        end_ms: segment.end_ms,
                        quote: segment.text.clone(),
                    }],
                    extractor_version: format!("{EXTRACTOR_VERSION}:{}:{:?}", rule.id, rule.mode)
                        .to_lowercase(),
                });
            }
        }
        results.sort_by_key(|proposal| proposal.action_id);
        results.dedup_by_key(|proposal| proposal.action_id);
        results
    }

    /// The model is deliberately confined to implicit/contextual judging. Explicit and hybrid
    /// trigger rules use the deterministic path above, including fuzzy matching and date parsing.
    fn derive_with_local_model(&self, request: &DerivationRequest) -> Result<Vec<ActionProposal>> {
        let candidates = self.model_candidates(request);
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_24b_model()?;
        let mut results = Vec::new();
        for batch in candidates.chunks(80) {
            let judged = self.judge_batch(request, batch)?;
            for judgment in judged.actions {
                if let Some(proposal) = self.validate_judgment(request, batch, judgment) {
                    results.push(proposal);
                }
            }
        }
        results.sort_by_key(|proposal| proposal.action_id);
        results.dedup_by_key(|proposal| proposal.action_id);
        Ok(results)
    }

    fn model_candidates(&self, request: &DerivationRequest) -> Vec<ModelCandidate> {
        let mut candidates = Vec::new();
        for segment in &request.segments {
            for rule in self.rules.iter().filter(|rule| rule.enabled) {
                let candidate = match rule.mode {
                    RuleMode::Implicit => implicit_prefilter(&segment.text),
                    RuleMode::Explicit | RuleMode::Hybrid => {
                        match_rule(rule, &segment.text).is_none()
                            && semantic_command_prefilter(rule, &segment.text)
                    }
                };
                if !candidate {
                    continue;
                }
                candidates.push(ModelCandidate {
                    rule_id: rule.id.clone(),
                    mode: format!("{:?}", rule.mode).to_lowercase(),
                    allowed_action: rule.action.clone(),
                    trigger_confidence: 1.0,
                    ordinal: segment.ordinal,
                    start_ms: segment.start_ms,
                    end_ms: segment.end_ms,
                    transcript_text: segment.text.clone(),
                    trigger_payload: segment.text.clone(),
                });
            }
        }
        candidates
    }

    fn ensure_24b_model(&self) -> Result<()> {
        let response = self
            .http
            .post(format!(
                "{}/api/show",
                self.ollama_url.trim_end_matches('/')
            ))
            .timeout(std::time::Duration::from_secs(10))
            .json(&serde_json::json!({"model":self.semantic_model}))
            .send()
            .context("contact the local Ollama server")?;
        let status = response.status();
        let body: serde_json::Value = response.json()?;
        if !status.is_success() {
            bail!("Ollama model inspection failed ({status}): {body}");
        }
        let parameters = body
            .pointer("/details/parameter_size")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if !is_required_local_model_size(parameters) {
            bail!(
                "semantic model {} is {parameters}, but Captain’s Log requires a local 24B model",
                self.semantic_model
            );
        }
        Ok(())
    }

    fn judge_batch(
        &self,
        request: &DerivationRequest,
        candidates: &[ModelCandidate],
    ) -> Result<JudgedActions> {
        let recorded = chrono::DateTime::from_timestamp_millis(request.recorded_at_ms)
            .unwrap_or_else(Utc::now);
        let input = serde_json::json!({
            "recording_id":request.recording_id,
            "recorded_at_utc":recorded.to_rfc3339(),
            "recorded_at_ms":request.recorded_at_ms,
            "timezone":request.timezone,
            "candidates":candidates,
        });
        let schema = serde_json::json!({
            "type":"object",
            "properties":{"actions":{"type":"array","items":{"type":"object","properties":{
                "rule_id":{"type":"string"},"evidence_ordinal":{"type":"integer"},
                "kind":{"type":"string","enum":["todo","reminder","calendar_event"]},
                "title":{"type":"string"},"due_at_ms":{"type":["integer","null"]},
                "original_time_phrase":{"type":["string","null"]},"confidence":{"type":"number"}
            },"required":["rule_id","evidence_ordinal","kind","title","due_at_ms","original_time_phrase","confidence"]}}},
            "required":["actions"]
        });
        let prompt = format!(
            "Act only as a routing judge for the supplied prefiltered candidates. Candidates may be explicit commands whose wording did not match the deterministic grammar, hybrid commands, or implicit commitments. Identify genuine requests made by the speaker and route them only to each candidate's allowed action. Deterministic code handles delivery, identity, policy, and validates dates. Never turn quoted, hypothetical, descriptive, or another person's speech into an action. Never invent a title, date, or time. Preserve any spoken relative date in original_time_phrase. Use the supplied recording time and timezone for relative dates. An undated todo/reminder must have null due_at_ms. Calendar events without a clear start time must be omitted. Return one evidence ordinal and its rule_id for every routed proposal. Output only the required JSON.\n\n{}",
            serde_json::to_string(&input)?
        );
        let response = self
            .http
            .post(format!("{}/api/chat", self.ollama_url.trim_end_matches('/')))
            .json(&serde_json::json!({
                "model":self.semantic_model,
                "stream":false,
                "format":schema,
                "options":{"temperature":0,"seed":7},
                "messages":[
                    {"role":"system","content":"You are the private semantic judge inside a local personal-data lakehouse. Follow the schema and preserve evidence."},
                    {"role":"user","content":prompt}
                ]
            }))
            .send()
            .context("run the local 24B semantic judge")?;
        let status = response.status();
        let body: serde_json::Value = response.json()?;
        if !status.is_success() {
            bail!("Ollama judge failed ({status}): {body}");
        }
        let content = body
            .pointer("/message/content")
            .and_then(serde_json::Value::as_str)
            .context("Ollama response has no structured message content")?;
        serde_json::from_str(content).context("decode 24B judge structured output")
    }

    fn validate_judgment(
        &self,
        request: &DerivationRequest,
        candidates: &[ModelCandidate],
        judgment: Judgment,
    ) -> Option<ActionProposal> {
        let candidate = candidates.iter().find(|candidate| {
            candidate.rule_id == judgment.rule_id && candidate.ordinal == judgment.evidence_ordinal
        })?;
        let rule = self.rules.iter().find(|rule| rule.id == judgment.rule_id)?;
        let title = if rule.action == "macos.reminder" {
            self.timed_reminder
                .captures(&candidate.transcript_text)
                .and_then(|capture| capture.name("payload"))
                .map(|payload| payload.as_str())
                .unwrap_or(&judgment.title)
        } else {
            &judgment.title
        }
        .trim_matches([' ', ',', '.'])
        .to_owned();
        if title.is_empty() || title.chars().count() > 300 {
            return None;
        }
        let kind = match rule.action.as_str() {
            "macos.reminder" => ActionKind::Reminder,
            "macos.calendar" => ActionKind::CalendarEvent,
            "macos.todo" => ActionKind::Todo,
            _ => judgment.kind,
        };
        let transcript_time = self.time.captures(&candidate.transcript_text);
        let parsed_due = transcript_time
            .as_ref()
            .and_then(|capture| {
                self.resolve_time(capture, request.recorded_at_ms, &request.timezone)
            })
            .or_else(|| {
                judgment
                    .original_time_phrase
                    .as_deref()
                    .and_then(|phrase| self.time.captures(phrase))
                    .and_then(|capture| {
                        self.resolve_time(&capture, request.recorded_at_ms, &request.timezone)
                    })
            });
        let due_at_ms = parsed_due.or(judgment.due_at_ms).filter(|due| {
            *due >= request.recorded_at_ms - Duration::days(1).num_milliseconds()
                && *due <= request.recorded_at_ms + Duration::days(3650).num_milliseconds()
        });
        if kind == ActionKind::CalendarEvent && due_at_ms.is_none() {
            return None;
        }
        let confidence = judgment
            .confidence
            .clamp(0.0, 1.0)
            .min(candidate.trigger_confidence);
        if confidence < rule.threshold {
            return None;
        }
        let identity = format!(
            "{}:{}:{}:{}:{title}:{due_at_ms:?}:{}",
            request.recording_id,
            request.transcript_run_id,
            candidate.ordinal,
            rule.id,
            self.semantic_model
        );
        Some(ActionProposal {
            action_id: Uuid::new_v5(&ACTION_NAMESPACE, identity.as_bytes()),
            recording_id: request.recording_id,
            transcript_run_id: request.transcript_run_id,
            kind,
            title,
            due_at_ms,
            original_time_phrase: transcript_time
                .and_then(|capture| capture.get(0).map(|value| value.as_str().to_owned()))
                .or(judgment.original_time_phrase),
            timezone: request.timezone.clone(),
            confidence,
            evidence: vec![SegmentEvidence {
                ordinal: candidate.ordinal,
                start_ms: candidate.start_ms,
                end_ms: candidate.end_ms,
                quote: candidate.transcript_text.clone(),
            }],
            extractor_version: format!(
                "local-24b:{}:{}:{:?}",
                self.semantic_model, rule.id, rule.mode
            )
            .to_lowercase(),
        })
    }

    fn auto_approve(&self, proposal: &ActionProposal) -> bool {
        self.rules.iter().any(|rule| {
            proposal
                .extractor_version
                .contains(&format!(":{}:", rule.id))
                && rule.approval == "auto_when_unambiguous"
                && proposal.confidence >= rule.threshold
                && (proposal.kind != ActionKind::CalendarEvent || proposal.due_at_ms.is_some())
        })
    }

    fn resolve_time(
        &self,
        capture: &regex::Captures<'_>,
        recorded_at_ms: i64,
        timezone: &str,
    ) -> Option<i64> {
        let tz: Tz = timezone.parse().ok().unwrap_or(chrono_tz::Europe::London);
        let recorded = Utc
            .timestamp_millis_opt(recorded_at_ms)
            .single()?
            .with_timezone(&tz);
        let day_phrase = capture.get(1)?.as_str().to_lowercase();
        let mut date = recorded.date_naive();
        if day_phrase == "tomorrow" {
            date += Duration::days(1);
        } else if let Some(name) = day_phrase.strip_prefix("next ") {
            let target = weekday_number(name)?;
            let current = recorded.weekday().num_days_from_monday() as i64;
            let mut delta = (target - current).rem_euclid(7);
            if delta == 0 {
                delta = 7;
            }
            date += Duration::days(delta);
        }
        let mut hour = capture
            .get(2)
            .and_then(|value| value.as_str().parse::<u32>().ok())
            .unwrap_or(9);
        let minute = capture
            .get(3)
            .and_then(|value| value.as_str().parse::<u32>().ok())
            .unwrap_or(0);
        match capture
            .get(4)
            .map(|value| value.as_str().to_lowercase())
            .as_deref()
        {
            Some("pm") if hour < 12 => hour += 12,
            Some("am") if hour == 12 => hour = 0,
            _ => {}
        }
        let naive = date.and_hms_opt(hour, minute, 0)?;
        let local = match tz.from_local_datetime(&naive) {
            LocalResult::Single(value) => value,
            LocalResult::Ambiguous(first, _) => first,
            LocalResult::None => tz
                .from_local_datetime(&(naive + Duration::hours(1)))
                .earliest()?,
        };
        Some(local.with_timezone(&Utc).timestamp_millis())
    }
}

fn implicit_prefilter(text: &str) -> bool {
    let normalized = format!(" {} ", text.to_lowercase().replace(['’', '\''], "'"));
    let first_person = [" i ", " i'm ", " i'll ", " we ", " we're ", " we'll "]
        .iter()
        .any(|marker| normalized.contains(marker));
    let commitment = [
        " need to ",
        " have to ",
        " should ",
        " must ",
        " plan to ",
        " going to ",
        " will ",
        " follow up ",
        " make sure ",
        " don't forget ",
    ]
    .iter()
    .any(|marker| normalized.contains(marker));
    first_person && commitment
}

fn semantic_command_prefilter(rule: &Rule, text: &str) -> bool {
    let normalized = text.to_lowercase().replace(['’', '\''], "'");
    match rule.action.as_str() {
        "macos.reminder" => normalized.contains("remind") || normalized.contains("remember"),
        "macos.todo" => normalized.contains("todo") || normalized.contains("to do"),
        "macos.calendar" => normalized.contains("calendar") || normalized.contains("schedule"),
        _ => false,
    }
}

fn default_rules() -> Vec<Rule> {
    vec![
        Rule {
            id: "reminders".into(),
            enabled: true,
            mode: RuleMode::Explicit,
            phrases: vec!["remind me to".into(), "remember to".into()],
            threshold: 0.78,
            action: "macos.reminder".into(),
            approval: "auto_when_unambiguous".into(),
        },
        Rule {
            id: "todos".into(),
            enabled: true,
            mode: RuleMode::Explicit,
            phrases: vec![
                "add to my todo".into(),
                "add to my to do".into(),
                "todo".into(),
            ],
            threshold: 0.78,
            action: "macos.todo".into(),
            approval: "auto_when_unambiguous".into(),
        },
        Rule {
            id: "calendar".into(),
            enabled: true,
            mode: RuleMode::Hybrid,
            phrases: vec![
                "for my calendar add".into(),
                "calendar add".into(),
                "schedule".into(),
            ],
            threshold: 0.78,
            action: "macos.calendar".into(),
            approval: "auto_when_unambiguous".into(),
        },
        Rule {
            id: "implicit-commitments".into(),
            enabled: false,
            mode: RuleMode::Implicit,
            phrases: vec![],
            threshold: 0.9,
            action: "proposal".into(),
            approval: "auto_when_unambiguous".into(),
        },
    ]
}

fn match_rule<'a>(rule: &Rule, text: &'a str) -> Option<(&'a str, f32)> {
    if rule.mode == RuleMode::Implicit {
        return None;
    }
    // Common natural form: “add buy milk to my todo”, where the payload interrupts the trigger.
    let lower = text.to_lowercase();
    if rule.action == "macos.todo"
        && let Some(after_add) = lower.find("add ")
        && let Some(todo) = lower[after_add + 4..].find(" to my to")
    {
        let start = after_add + 4;
        return Some((text[start..start + todo].trim(), 0.98));
    }
    let words: Vec<_> = text.split_whitespace().collect();
    let normalized_words: Vec<_> = words.iter().map(|word| normalize(word)).collect();
    let mut best: Option<(usize, usize, f32)> = None;
    for phrase in &rule.phrases {
        let phrase_words: Vec<_> = phrase.split_whitespace().map(normalize).collect();
        for start in 0..normalized_words.len() {
            for length in phrase_words.len().saturating_sub(1)..=phrase_words.len() + 1 {
                if length == 0 || start + length > normalized_words.len() {
                    continue;
                }
                let candidate = normalized_words[start..start + length].join(" ");
                let expected = phrase_words.join(" ");
                let score = similarity(&candidate, &expected);
                if score >= rule.threshold && best.is_none_or(|(_, _, old)| score > old) {
                    best = Some((start, length, score));
                }
            }
        }
    }
    let (start, length, score) = best?;
    let remainder = words[start + length..].join(" ");
    if remainder.trim().is_empty() {
        return None;
    }
    // Return a slice when possible so evidence keeps the original spelling/case.
    let offset = text.find(&remainder)?;
    Some((text[offset..].trim(), score))
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn similarity(left: &str, right: &str) -> f32 {
    let left: Vec<_> = left.chars().collect();
    let right: Vec<_> = right.chars().collect();
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (i, a) in left.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, b) in right.iter().enumerate() {
            current.push(
                (previous[j + 1] + 1)
                    .min(current[j] + 1)
                    .min(previous[j] + usize::from(a != b)),
            );
        }
        previous = current;
    }
    1.0 - previous[right.len()] as f32 / left.len().max(right.len()) as f32
}

fn weekday_number(name: &str) -> Option<i64> {
    Some(match name {
        "monday" => 0,
        "tuesday" => 1,
        "wednesday" => 2,
        "thursday" => 3,
        "friday" => 4,
        "saturday" => 5,
        "sunday" => 6,
        _ => return None,
    })
}

fn request_from_transcript(run: &TranscriptCompleted, recorded_at_ms: i64) -> DerivationRequest {
    DerivationRequest {
        recording_id: run.recording_id,
        transcript_run_id: run.run_id,
        recorded_at_ms: if run.recorded_at_ms > 0 {
            run.recorded_at_ms
        } else {
            recorded_at_ms
        },
        segments: run.segments.clone(),
        timezone: std::env::var("PERSONAL_AUDIO_LAKE_TIMEZONE")
            .unwrap_or_else(|_| "Etc/UTC".into()),
    }
}

fn emit(envelope: &Envelope) -> Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(envelope.to_ndjson()?.as_bytes())?;
    stdout.flush()?;
    Ok(())
}

fn main() -> Result<()> {
    let extractor = Extractor::configured();
    let readiness = extractor.ensure_24b_model();
    emit(&Envelope::root(Message::WorkerHealth(WorkerHealth {
        role: WorkerRole::Derive,
        state: if readiness.is_ok() {
            HealthState::Ready
        } else {
            HealthState::Degraded
        },
        detail: readiness.map_or_else(
            |error| format!("local 24B model unavailable: {error:#}"),
            |_| format!("local 24B model ready: {}", extractor.semantic_model),
        ),
        backlog: 0,
    })))?;
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let envelope = match Envelope::from_json(&line) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("invalid contract: {error}");
                continue;
            }
        };
        let request = match &envelope.message {
            Message::DerivationRequested(value) => Some(value.clone()),
            Message::TranscriptCompleted(run) => {
                Some(request_from_transcript(run, envelope.emitted_at_ms))
            }
            _ => None,
        };
        if let Some(request) = request {
            let extractor = Extractor::configured();
            let mut proposals = extractor.derive(&request);
            match extractor.derive_with_local_model(&request) {
                Ok(implicit) => proposals.extend(implicit),
                Err(error) => {
                    emit(&Envelope::caused_by(
                        &envelope,
                        Message::Failure(Failure {
                            worker: WorkerRole::Derive,
                            code: "local_24b_judge_failed".into(),
                            detail: error.to_string(),
                            retryable: true,
                            failed_message_id: Some(envelope.id),
                        }),
                    ))?;
                }
            }
            proposals.sort_by_key(|proposal| proposal.action_id);
            proposals.dedup_by_key(|proposal| proposal.action_id);
            let completed = DerivationCompleted {
                recording_id: request.recording_id,
                transcript_run_id: request.transcript_run_id,
                actions: proposals
                    .iter()
                    .map(|proposal| DerivedAction {
                        action_id: proposal.action_id,
                        kind: proposal.kind,
                        title: proposal.title.clone(),
                        auto_approved: extractor.auto_approve(proposal),
                    })
                    .collect(),
            };
            for proposal in proposals {
                let proposed =
                    Envelope::caused_by(&envelope, Message::ActionProposed(proposal.clone()));
                emit(&proposed)?;
                if extractor.auto_approve(&proposal) {
                    emit(&Envelope::caused_by(
                        &proposed,
                        Message::ActionApproved {
                            action_id: proposal.action_id,
                        },
                    ))?;
                }
            }
            emit(&Envelope::caused_by(
                &envelope,
                Message::DerivationCompleted(completed),
            ))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use personal_audio_lake_contracts::{TranscriptSegment, assert_contract_fixtures};

    #[test]
    fn independently_accepts_contract_fixtures() {
        assert_contract_fixtures();
    }

    #[test]
    fn derives_london_reminder_with_stable_identity() {
        let request = DerivationRequest {
            recording_id: Uuid::new_v4(),
            transcript_run_id: Uuid::new_v4(),
            recorded_at_ms: 1_783_938_600_000,
            segments: vec![TranscriptSegment {
                ordinal: 0,
                start_ms: 100,
                end_ms: 2_000,
                speaker: None,
                text: "Remind me to call Jess tomorrow at 9am.".into(),
                confidence: Some(0.95),
                language: Some("en".into()),
            }],
            timezone: "Europe/London".into(),
        };
        let first = Extractor::new().derive(&request);
        let second = Extractor::new().derive(&request);
        assert_eq!(first, second);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].title, "call Jess");
        assert_eq!(first[0].kind, ActionKind::Reminder);
        assert!(first[0].due_at_ms.is_some());
    }

    #[test]
    fn tolerates_a_small_transcription_error_in_trigger() {
        let extractor = Extractor::new();
        let rule = &extractor.rules[0];
        let (title, score) = match_rule(rule, "Reminder me to call Jess tomorrow").unwrap();
        assert_eq!(title, "call Jess tomorrow");
        assert!(score >= rule.threshold);
    }

    #[test]
    fn local_model_fallback_accepts_time_inside_reminder_trigger() {
        let extractor = Extractor::new();
        let recording_id = Uuid::new_v4();
        let transcript_run_id = Uuid::new_v4();
        let text = "Remind me tomorrow to send the project update after the morning meeting.";
        let request = DerivationRequest {
            recording_id,
            transcript_run_id,
            recorded_at_ms: 1_783_978_292_000,
            segments: vec![TranscriptSegment {
                ordinal: 0,
                start_ms: 0,
                end_ms: 10_000,
                speaker: None,
                text: text.into(),
                confidence: None,
                language: Some("en".into()),
            }],
            timezone: "Europe/London".into(),
        };
        let candidates = extractor.model_candidates(&request);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].rule_id, "reminders");

        let proposal = extractor
            .validate_judgment(
                &request,
                &candidates,
                Judgment {
                    rule_id: "reminders".into(),
                    evidence_ordinal: 0,
                    kind: ActionKind::Reminder,
                    title: "send the project update after the morning meeting".into(),
                    due_at_ms: None,
                    original_time_phrase: Some("tomorrow".into()),
                    confidence: 0.95,
                },
            )
            .unwrap();
        assert_eq!(proposal.kind, ActionKind::Reminder);
        assert_eq!(
            proposal.title,
            "send the project update after the morning meeting"
        );
        assert_eq!(proposal.original_time_phrase.as_deref(), Some("tomorrow"));
        assert!(
            proposal
                .due_at_ms
                .is_some_and(|due| due > request.recorded_at_ms)
        );
        assert!(extractor.auto_approve(&proposal));
    }

    #[test]
    fn recognizes_payload_before_todo_suffix() {
        let extractor = Extractor::new();
        let (title, _) = match_rule(&extractor.rules[1], "Add buy milk to my todo").unwrap();
        assert_eq!(title, "buy milk");
    }

    #[test]
    fn implicit_model_prefilter_rejects_descriptive_speech() {
        assert!(!implicit_prefilter(
            "The team described a calendar integration in the meeting"
        ));
        assert!(implicit_prefilter("I should follow up with Jess next week"));
    }

    #[test]
    fn every_enabled_rule_uses_ask_forgiveness_delivery() {
        assert!(
            default_rules()
                .iter()
                .filter(|rule| rule.enabled)
                .all(|rule| rule.approval == "auto_when_unambiguous")
        );
    }
}
