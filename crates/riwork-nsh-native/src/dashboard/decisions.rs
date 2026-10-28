//! Dashboard responses use the existing host and an exact, freshly re-read
//! provider request. A summary or a JS-supplied request body cannot authorize one.
use crate::{
    chat::{dto::{ApprovalDto, ChatPanelDto, ChoiceDto, OptionDto, PromptDto, QuestionDto}, view},
    service,
};
use nsh_core::{
    JsonValue,
    element::{JsHost, JsIn, JsOwned, JsValue, PropError, TsTypes},
};
use riwork_engine::{
    chat::{
        client::{CallError, Client, socket_path},
        log::read_snapshot,
        model::{ApprovalKind, ChatCommand, ChatEvent, ChatState, Decision},
    },
    global_overview::observed::{self, Request, Source},
};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::{Mutex, OnceLock}};

const CHANGED: &str = "This request changed or closed. Refresh the dashboard and review it again.";
const MAX_REQUEST_BYTES: usize = 32 << 10;
const MAX_REVIEWS: usize = 512;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardDecisionReviewDto {
    pub review_id: String,
    pub chat_id: String,
    pub request_id: String,
    pub title: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<QuestionDto>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl JsValue for DashboardDecisionReviewDto {
    fn ts_type(types: &mut TsTypes) -> String {
        ChatPanelDto::ts_type(types);
        types.declare("DashboardDecisionReview", |_| "export interface DashboardDecisionReview { reviewId: string; chatId: string; requestId: string; title: string; kind: 'approval' | 'question'; approval?: ChatApproval; question?: ChatQuestion; status: 'pending' | 'sending' | 'sent' | 'uncertain' | 'unavailable'; error?: string; }".into())
    }
    fn from_js(value: JsIn<'_>) -> Result<Self, PropError> {
        serde_json::from_value(JsonValue::from_js(value)?)
            .map_err(|error| PropError(format!("invalid: {error}")))
    }
    fn to_js<'a>(&self, host: &'a dyn JsHost) -> Result<JsOwned<'a>, PropError> {
        serde_json::to_value(self).map_err(|error| PropError(error.to_string()))?.to_js(host)
    }
}

#[derive(Clone)]
struct Review {
    home: PathBuf,
    source: Source,
    request: Request,
    dto: DashboardDecisionReviewDto,
}

// Shared across windows. In-flight, sent and uncertain responses are retained:
// closing a dialog must not turn an unknown delivery into permission to resend.
static REVIEWS: OnceLock<Mutex<Vec<Review>>> = OnceLock::new();
fn reviews() -> &'static Mutex<Vec<Review>> {
    REVIEWS.get_or_init(|| Mutex::new(Vec::new()))
}

pub(super) fn review(
    chat_id: String,
    request_id: String,
    kind: Option<String>,
) -> Result<DashboardDecisionReviewDto, String> {
    uuid::Uuid::parse_str(&chat_id).map_err(|_| "invalid: chat id")?;
    if request_id.is_empty() || request_id.len() > 4096 {
        return Err("invalid: request id".into());
    }
    if kind.as_deref().is_some_and(|kind| !matches!(kind, "approval" | "question")) {
        return Err("invalid: request kind".into());
    }
    let home = service::process_home()?;
    let mut client = Client::connect(&socket_path(&home))?;
    let info = client.list()?.into_iter().find(|chat| chat.id == chat_id)
        .ok_or("This chat is no longer available.")?;
    if info.state != ChatState::Waiting {
        return Err("This chat no longer waits for an answer. Refresh the dashboard.".into());
    }
    let fresh = read_snapshot(&home, &chat_id, None, u64::MAX, 1, 8 << 20, &[])
        .map_err(|error| format!("This request could not be read ({error}). Open the chat."))?;
    let requests: Vec<Request> = fresh.controls.into_iter().filter_map(|event| match event {
        ChatEvent::ApprovalRequested { approval }
            if approval.request_id == request_id && kind.as_deref() != Some("question") =>
                Some(Request::Approval { approval }),
        ChatEvent::QuestionRequested { question }
            if question.request_id == request_id && kind.as_deref() != Some("approval") =>
                Some(Request::Question { question }),
        _ => None,
    }).collect();
    if requests.len() != 1 {
        return Err(CHANGED.into());
    }
    let request = requests.into_iter().next().ok_or(CHANGED)?;
    if serde_json::to_vec(&request).map_err(|error| error.to_string())?.len() > MAX_REQUEST_BYTES {
        return Err("This request is too large for the dashboard review. Open the chat.".into());
    }
    let source = observed::open_request(&home, &chat_id, &request).ok_or(CHANGED)?;
    let mut entries = reviews().lock().unwrap_or_else(|error| error.into_inner());
    if let Some(existing) = entries.iter().find(|entry|
        entry.home == home && entry.source == source && entry.request == request
            && entry.dto.status != "unavailable") {
        return Ok(existing.dto.clone());
    }
    // A newly observed source retires earlier completed reviews for this exact
    // chat/request. Never retire an in-flight response or forget its outcome.
    entries.retain(|entry| entry.dto.status == "sending" || !(entry.home == home
        && entry.dto.chat_id == chat_id && entry.dto.request_id == request_id
        && request_kind(&entry.request) == request_kind(&request)
        && (entry.source != source || entry.dto.status == "unavailable")));
    if entries.len() >= MAX_REVIEWS {
        if let Some(index) = entries.iter().position(|entry|
            matches!(entry.dto.status.as_str(), "pending" | "unavailable")) {
            entries.remove(index);
        } else {
            return Err("Dashboard review history is full. Answer this request in the chat.".into());
        }
    }
    let (approval, question) = request_dto(&request);
    let dto = DashboardDecisionReviewDto {
        review_id: uuid::Uuid::new_v4().to_string(), chat_id, request_id,
        title: info.title, kind: request_kind(&request).into(), approval, question,
        status: "pending".into(), error: None,
    };
    entries.push(Review { home, source, request, dto: dto.clone() });
    Ok(dto)
}

pub(super) fn respond(
    review_id: String,
    decision: Option<String>,
    answers: Option<Vec<Vec<String>>>,
) -> Result<DashboardDecisionReviewDto, String> {
    let (expected, command) = {
        let mut entries = reviews().lock().unwrap_or_else(|error| error.into_inner());
        let entry = entries.iter_mut().find(|entry| entry.dto.review_id == review_id)
            .ok_or("This review expired. Read the request again before answering.")?;
        if entry.dto.status != "pending" {
            return Ok(entry.dto.clone());
        }
        let command = match make_command(&entry.request, decision, answers) {
            Ok(command) => command,
            Err(message) => {
                entry.dto.error = Some(message);
                return Ok(entry.dto.clone());
            }
        };
        entry.dto.status = "sending".into();
        entry.dto.error = None;
        (entry.clone(), command)
    };
    let result = send_checked(&expected, command);
    let mut entries = reviews().lock().unwrap_or_else(|error| error.into_inner());
    let entry = entries.iter_mut().find(|entry| entry.dto.review_id == review_id)
        .ok_or("The response may have gone through. Open the chat to check its state.")?;
    match result {
        Ok(()) => entry.dto.status = "sent".into(),
        Err(failure) => {
            entry.dto.status = failure.status.into();
            entry.dto.error = Some(failure.message);
        }
    }
    Ok(entry.dto.clone())
}

fn make_command(
    request: &Request,
    decision: Option<String>,
    answers: Option<Vec<Vec<String>>>,
) -> Result<ChatCommand, String> {
    match request {
        Request::Approval { approval } if answers.is_none() => {
            let choice = match decision.as_deref() {
                Some("accept") => Some(Decision::Accept),
                Some("accept_for_session") => Some(Decision::AcceptForSession),
                Some("decline") => Some(Decision::Decline),
                Some("cancel") => Some(Decision::Cancel),
                _ => None,
            }.filter(|choice| approval.choices.contains(choice))
                .ok_or("Choose one of the provider's decisions.")?;
            Ok(ChatCommand::Approve { request_id: approval.request_id.clone(), decision: choice })
        }
        Request::Question { question } if decision.is_none() => {
            let answers = answers.ok_or("Answer every question before sending.")?;
            if answers.len() != question.questions.len() || answers.is_empty()
                || answers.iter().any(|answer| answer.is_empty()
                    || answer.iter().any(|text| text.trim().is_empty())) {
                return Err("Answer every question before sending.".into());
            }
            if answers.iter().flatten().map(String::len).sum::<usize>() > 64 << 10 {
                return Err("These answers are too large for the dashboard. Open the chat.".into());
            }
            Ok(ChatCommand::Answer { request_id: question.request_id.clone(), answers })
        }
        _ => Err("Choose one of the provider's decisions or answer every question.".into()),
    }
}

struct SendFailure { status: &'static str, message: String }
impl From<String> for SendFailure {
    fn from(message: String) -> Self { Self { status: "pending", message } }
}
fn unavailable(message: &str) -> SendFailure {
    SendFailure { status: "unavailable", message: message.into() }
}
fn send_checked(expected: &Review, command: ChatCommand) -> Result<(), SendFailure> {
    // This connects to an existing host; it never resumes a provider or starts one.
    let mut client = Client::connect(&socket_path(&expected.home))?;
    let info = client.list()?.into_iter().find(|chat| chat.id == expected.dto.chat_id)
        .ok_or_else(|| unavailable("This chat is no longer available."))?;
    if info.state != ChatState::Waiting {
        return Err(unavailable("This chat no longer waits for an answer. Refresh the dashboard."));
    }
    let fresh = read_snapshot(&expected.home, &expected.dto.chat_id, None, u64::MAX, 1, 8 << 20, &[])
        .map_err(|error| format!("This chat could not be checked again ({error}). Answer it in the chat."))?;
    let matches = fresh.controls.iter().any(|event| match (&expected.request, event) {
        (Request::Approval { approval: expected }, ChatEvent::ApprovalRequested { approval }) => expected == approval,
        (Request::Question { question: expected }, ChatEvent::QuestionRequested { question }) => expected == question,
        _ => false,
    });
    if !matches || observed::open_request(&expected.home, &expected.dto.chat_id, &expected.request).as_ref()
        != Some(&expected.source) {
        return Err(unavailable(CHANGED));
    }
    client.command_checked(&expected.dto.chat_id, command).map_err(|error| match error {
        CallError::Refused(message) => message.into(),
        CallError::Broken(message) => SendFailure {
            status: "uncertain",
            message: format!("The response may have gone through. Check the chat before sending again. {message}"),
        },
    })
}

fn request_kind(request: &Request) -> &'static str {
    match request { Request::Approval { .. } => "approval", Request::Question { .. } => "question" }
}
fn request_dto(request: &Request) -> (Option<ApprovalDto>, Option<QuestionDto>) {
    match request {
        Request::Approval { approval } => (Some(ApprovalDto {
            request_id: approval.request_id.clone(),
            kind: match approval.kind {
                ApprovalKind::Command => "command", ApprovalKind::FileChange => "file_change",
                ApprovalKind::Permissions => "permissions", ApprovalKind::Tool => "tool",
            }.into(),
            heading: view::kind_label(approval.kind).into(),
            title: approval.title.clone(), detail: approval.detail.clone(),
            choices: approval.choices.iter().map(|choice| ChoiceDto {
                decision: match choice {
                    Decision::Accept => "accept", Decision::AcceptForSession => "accept_for_session",
                    Decision::Decline => "decline", Decision::Cancel => "cancel",
                }.into(),
                label: view::decision_label(*choice).into(),
                // Opening review focuses Close; Return must never implicitly allow.
                key: None,
            }).collect(),
        }), None),
        Request::Question { question } => (None, Some(QuestionDto {
            request_id: question.request_id.clone(),
            prompts: question.questions.iter().map(|prompt| PromptDto {
                header: prompt.header.clone(), question: prompt.question.clone(),
                options: prompt.options.iter().map(|option| OptionDto {
                    label: option.label.clone(), description: option.description.clone(),
                }).collect(), multi_select: prompt.multi_select,
            }).collect(),
        })),
    }
}
