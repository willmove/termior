use futures::StreamExt;
use gpui::{
    canvas, div, prelude::*, px, App, Bounds, Context, EventEmitter, FocusHandle, Focusable,
    InputHandler, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Point, SharedString,
    UTF16Selection, WeakEntity, Window,
};
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use termior_ai::{
    Agent, AgentDefinition, AgentDefinitionStore, AgentOutcome, ApprovalDecision, ApprovalRequest,
    AttachmentSource, ComposerDraft, EditProposalSummary, HttpProvider, KeyringSecretStore,
    Message, ProjectMemory, ProviderConfig, Role, SecretStore, SessionStore, SnippetStore,
    TerminalContext, TerminalContextProvider, ToolExecutor, ToolRegistry,
};
use termior_platform::AgentStatus;
use termior_security::workspace::WorkspaceAuthRegistry;
use termior_store::{DataFiles, Settings};

#[derive(Clone)]
struct AgentRuntime {
    config: ProviderConfig,
    tools: ToolRegistry,
    executor: Arc<ToolExecutor>,
    system_prompt: String,
}

struct PendingApproval {
    request: ApprovalRequest,
    history: Vec<Message>,
}

struct PendingEdit {
    summary: EditProposalSummary,
    decisions: HashMap<usize, bool>,
}

struct LiveTerminalContext {
    snapshot: Mutex<TerminalContext>,
}

impl TerminalContextProvider for LiveTerminalContext {
    fn snapshot(&self) -> TerminalContext {
        self.snapshot.lock().unwrap().clone()
    }
}

pub struct ComposerView {
    draft: ComposerDraft,
    cursor: usize,
    marked_text: String,
    focus_handle: FocusHandle,
    history: Vec<Message>,
    plan_mode: bool,
    plan_confirmed: bool,
    awaiting_plan_confirmation: bool,
    busy: bool,
    status: String,
    runtime: Option<AgentRuntime>,
    pending_approval: Option<PendingApproval>,
    pending_edit: Option<PendingEdit>,
    sessions: SessionStore,
    snippets: SnippetStore,
    custom_agents: Vec<AgentDefinition>,
    active_custom_agent: Option<usize>,
    base_system_prompt: String,
    full_tools: Option<ToolRegistry>,
    data_dir: Option<PathBuf>,
    terminal_context: Arc<LiveTerminalContext>,
}

impl ComposerView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            draft: ComposerDraft::default(),
            cursor: 0,
            marked_text: String::new(),
            focus_handle: cx.focus_handle(),
            history: Vec::new(),
            plan_mode: false,
            plan_confirmed: false,
            awaiting_plan_confirmation: false,
            busy: false,
            status: "Choose a default chat model in Settings → Models".into(),
            runtime: None,
            pending_approval: None,
            pending_edit: None,
            sessions: SessionStore::default(),
            snippets: SnippetStore::default(),
            custom_agents: Vec::new(),
            active_custom_agent: None,
            base_system_prompt: String::new(),
            full_tools: None,
            data_dir: None,
            terminal_context: Arc::new(LiveTerminalContext {
                snapshot: Mutex::new(TerminalContext {
                    cwd: String::new(),
                    recent_output: String::new(),
                    captured_at_unix_ms: 0,
                }),
            }),
        }
    }

    pub fn configure(
        &mut self,
        settings: &Settings,
        root: &Path,
        workspace_auth: WorkspaceAuthRegistry,
        data_dir: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.data_dir = data_dir;
        if let Some(dir) = &self.data_dir {
            let files = DataFiles::new(dir.clone());
            self.sessions = files.sessions::<SessionStore>().load().unwrap_or_default();
            self.snippets = files.snippets::<SnippetStore>().load().unwrap_or_default();
            self.custom_agents = files
                .agents::<AgentDefinitionStore>()
                .load()
                .unwrap_or_default()
                .agents;
            if self
                .active_custom_agent
                .is_some_and(|index| index >= self.custom_agents.len())
            {
                self.active_custom_agent = None;
            }
        }
        if self.sessions.sessions.is_empty() {
            self.sessions.create(new_session_id());
        }
        self.history = self
            .sessions
            .active_id
            .as_deref()
            .and_then(|id| self.sessions.get(id))
            .map(|session| session.messages.clone())
            .unwrap_or_default();

        let profile = settings
            .models
            .active_chat_profile
            .as_deref()
            .and_then(|id| {
                settings
                    .models
                    .profiles
                    .iter()
                    .find(|profile| profile.id == id)
            })
            .filter(|profile| profile.enabled)
            .cloned();
        let Some(profile) = profile else {
            self.runtime = None;
            self.status = "Choose and enable a default chat model in Settings → Models".into();
            cx.notify();
            return;
        };
        let key_name = format!("provider:{}", profile.id);
        let api_key = KeyringSecretStore::new().get(&key_name).ok().flatten();
        if !profile.local && api_key.is_none() {
            self.runtime = None;
            self.status = format!(
                "Add an API key for {} in Settings → Models",
                profile.display_name
            );
            cx.notify();
            return;
        }
        let config = match ProviderConfig::from_settings(&profile, api_key) {
            Ok(config) => config,
            Err(error) => {
                self.runtime = None;
                self.status = error.to_string();
                cx.notify();
                return;
            }
        };
        let full_tools = ToolRegistry::new(workspace_auth);
        let executor = match ToolExecutor::new(root, full_tools.clone()) {
            Ok(executor) => Arc::new(executor.with_terminal_context(self.terminal_context.clone())),
            Err(error) => {
                self.runtime = None;
                self.status = error.to_string();
                cx.notify();
                return;
            }
        };
        let memory = ProjectMemory::load_from(root)
            .map(|memory| {
                format!(
                    "\n\nProject memory ({}):\n{}",
                    memory.source, memory.content
                )
            })
            .unwrap_or_default();
        let base_system_prompt = format!(
            "You are Termior's coding agent. Work only inside the authorized workspace. Read before editing, use tools when needed, and never claim an action succeeded without its tool result.{}{}",
            if settings.custom_instructions.trim().is_empty() {
                String::new()
            } else {
                format!("\n\nGlobal instructions:\n{}", settings.custom_instructions)
            },
            memory
        );
        let (tools, system_prompt) =
            match self.selected_agent_runtime(&full_tools, &base_system_prompt) {
                Ok(runtime) => runtime,
                Err(error) => {
                    self.runtime = None;
                    self.status = error;
                    cx.notify();
                    return;
                }
            };
        self.full_tools = Some(full_tools);
        self.base_system_prompt = base_system_prompt;
        self.runtime = Some(AgentRuntime {
            config,
            tools,
            executor,
            system_prompt,
        });
        self.status = format!(
            "Ready · {} / {} · {}",
            profile.display_name,
            profile.model,
            self.active_agent_name()
        );
        cx.notify();
    }

    fn selected_agent_runtime(
        &self,
        full_tools: &ToolRegistry,
        base_prompt: &str,
    ) -> Result<(ToolRegistry, String), String> {
        let Some(index) = self.active_custom_agent else {
            return Ok((full_tools.clone(), base_prompt.to_owned()));
        };
        let definition = self
            .custom_agents
            .get(index)
            .ok_or_else(|| "Selected custom agent no longer exists".to_owned())?;
        let tools = full_tools
            .clone()
            .subset(definition.tools.clone())
            .map_err(|error| format!("Custom agent tools are invalid: {error}"))?;
        Ok((
            tools,
            format!(
                "{base_prompt}\n\nActive custom agent ({}):\n{}",
                definition.name, definition.system_prompt
            ),
        ))
    }

    fn active_agent_name(&self) -> &str {
        self.active_custom_agent
            .and_then(|index| self.custom_agents.get(index))
            .map(|agent| agent.name.as_str())
            .unwrap_or("Built-in Agent")
    }

    fn cycle_custom_agent(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.custom_agents.is_empty() {
            return;
        }
        self.active_custom_agent = match self.active_custom_agent {
            None => Some(0),
            Some(index) if index + 1 < self.custom_agents.len() => Some(index + 1),
            Some(_) => None,
        };
        if let Some(full_tools) = self.full_tools.clone() {
            match self.selected_agent_runtime(&full_tools, &self.base_system_prompt) {
                Ok((tools, prompt)) => {
                    if let Some(runtime) = self.runtime.as_mut() {
                        runtime.tools = tools;
                        runtime.system_prompt = prompt;
                    }
                    self.status = format!("Active agent: {}", self.active_agent_name());
                }
                Err(error) => self.status = error,
            }
        }
        cx.notify();
    }

    pub fn attach_selection(&mut self, source: AttachmentSource, label: String, text: String) {
        self.draft.attach_selection(source, label, text);
    }

    pub fn attach_file(&mut self, path: PathBuf) {
        self.draft.attach_file(path);
    }

    pub fn update_terminal_context(&mut self, cwd: String, recent_output: String) {
        let captured_at_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        *self.terminal_context.snapshot.lock().unwrap() = TerminalContext {
            cwd,
            recent_output,
            captured_at_unix_ms,
        };
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.pending_approval.is_some() || self.pending_edit.is_some() {
            return;
        }
        let Some(runtime) = self.runtime.clone() else {
            self.status = "Configure a default model in Settings → Models first".into();
            cx.notify();
            return;
        };
        let mut payload = match self.draft.build_payload(&runtime.tools) {
            Ok(payload) => payload,
            Err(error) => {
                self.status = format!("Attachment rejected: {error}");
                cx.notify();
                return;
            }
        };
        payload.text = self.snippets.expand(&payload.text);
        if !payload.images.is_empty() {
            payload.text.push_str(&format!(
                "\n\n[{} image attachment(s) are available to multimodal providers]",
                payload.images.len()
            ));
        }
        if payload.text.trim().is_empty() {
            return;
        }
        self.history.push(Message::user(payload.text));
        self.draft.input.clear();
        self.draft.attachments.clear();
        self.cursor = 0;
        self.busy = true;
        self.status = "Agent is working…".into();
        cx.emit(AgentStatus::Working);
        self.persist_session();

        let history = self.history.clone();
        let plan_request = self.plan_mode && !self.plan_confirmed;
        cx.spawn(async move |view, cx| {
            // 增量事件 channel：run_agent 在后台线程逐条推 ChatEvent，主线程边收边渲染。
            let (event_tx, mut event_rx) =
                futures::channel::mpsc::unbounded::<termior_ai::ChatEvent>();
            let task = cx
                .background_executor()
                .spawn(async move { run_agent(runtime, history, plan_request, event_tx).await });
            // 主线程消费增量：每收到一个 TextDelta 就追加到当前 assistant 草稿并重绘。
            while let Some(event) = event_rx.next().await {
                if let termior_ai::ChatEvent::TextDelta(delta) = event {
                    let _ = view.update(cx, |view, cx| {
                        view.append_text_delta(delta);
                        cx.notify();
                    });
                }
            }
            let result = task.await;
            let _ = view.update(cx, |view, cx| {
                view.busy = false;
                view.apply_agent_result(result, cx);
                if plan_request && view.pending_approval.is_none() {
                    view.awaiting_plan_confirmation = true;
                    view.status =
                        "Plan ready · confirm or reject before any write-capable tool is exposed"
                            .into();
                    cx.emit(AgentStatus::Attention);
                } else if !plan_request {
                    view.plan_confirmed = false;
                    view.awaiting_plan_confirmation = false;
                }
                view.persist_session();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn apply_agent_result(&mut self, result: Result<AgentOutcome, String>, cx: &mut Context<Self>) {
        match result {
            Ok(outcome) => {
                self.history = outcome.messages.clone();
                if let Some(request) = outcome.pending_approval {
                    self.status = format!("Approval required: {}", request.summary);
                    self.pending_approval = Some(PendingApproval {
                        request,
                        history: outcome.messages,
                    });
                    cx.emit(AgentStatus::Attention);
                } else {
                    self.status = "Agent finished".into();
                    cx.emit(AgentStatus::Finished);
                }
            }
            Err(error) => {
                self.status = format!("Agent error: {error}");
                cx.emit(AgentStatus::Error);
            }
        }
    }

    fn approve_tool(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(pending) = self.pending_approval.take() else {
            return;
        };
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        self.busy = true;
        self.status = format!("Running approved tool: {}", pending.request.tool_name);
        cx.emit(AgentStatus::Working);
        cx.spawn(async move |view, cx| {
            // 增量事件 channel：resume_agent 在后台线程逐条推 ChatEvent。
            let (event_tx, mut event_rx) =
                futures::channel::mpsc::unbounded::<termior_ai::ChatEvent>();
            let task = cx
                .background_executor()
                .spawn(async move { resume_agent(runtime, pending, event_tx).await });
            while let Some(event) = event_rx.next().await {
                if let termior_ai::ChatEvent::TextDelta(delta) = event {
                    let _ = view.update(cx, |view, cx| {
                        view.append_text_delta(delta);
                        cx.notify();
                    });
                }
            }
            let result = task.await;
            let _ = view.update(cx, |view, cx| {
                view.busy = false;
                match result {
                    Ok((outcome, edit)) => {
                        view.apply_agent_result(Ok(outcome), cx);
                        if let Some(summary) = edit {
                            view.pending_edit = Some(PendingEdit {
                                summary,
                                decisions: HashMap::new(),
                            });
                            view.status = "Review every proposed hunk before writing".into();
                            cx.emit(AgentStatus::Attention);
                        }
                    }
                    Err(error) => {
                        view.status = format!("Tool failed: {error}");
                        cx.emit(AgentStatus::Error);
                    }
                }
                view.persist_session();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn reject_tool(&mut self, cx: &mut Context<Self>) {
        if let Some(pending) = self.pending_approval.take() {
            self.history = pending.history;
            self.history.push(Message::assistant(format!(
                "The user rejected the {} tool call.",
                pending.request.tool_name
            )));
            self.status = "Tool call rejected".into();
            cx.emit(AgentStatus::Finished);
            self.persist_session();
            cx.notify();
        }
    }

    /// 追加流式文本增量：把 TextDelta 实时拼到当前 assistant 消息草稿上。
    /// 若 history 末尾不是 assistant，先 push 一条空 assistant 作为流式占位。
    /// 最终消息会在 apply_agent_result 时被 AgentOutcome.messages 整体覆盖。
    fn append_text_delta(&mut self, delta: String) {
        let needs_new = matches!(self.history.last(), Some(m) if m.role != Role::Assistant);
        if needs_new {
            self.history.push(Message::assistant(String::new()));
        }
        if let Some(last) = self.history.last_mut() {
            last.content.push_str(&delta);
        }
    }

    fn decide_hunk(&mut self, id: usize, accept: bool, cx: &mut Context<Self>) {
        if let Some(edit) = self.pending_edit.as_mut() {
            edit.decisions.insert(id, accept);
            cx.notify();
        }
    }

    fn apply_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.pending_edit.take() else {
            return;
        };
        if edit.decisions.len() != edit.summary.hunk_ids.len() {
            self.status = "Accept or reject every hunk first".into();
            self.pending_edit = Some(edit);
            cx.notify();
            return;
        }
        let accepted = edit
            .summary
            .hunk_ids
            .iter()
            .filter(|id| edit.decisions.get(id) == Some(&true))
            .copied()
            .collect::<Vec<_>>();
        match self.runtime.as_ref() {
            Some(runtime) => match runtime.executor.accept_edit(&edit.summary.id, &accepted) {
                Ok(message) => {
                    self.status = message;
                    cx.emit(AgentStatus::Finished);
                }
                Err(error) => {
                    self.status = format!("Edit failed: {error}");
                    cx.emit(AgentStatus::Error);
                }
            },
            None => {
                self.status = "No active edit executor".into();
                cx.emit(AgentStatus::Error);
            }
        }
        cx.notify();
    }

    fn reject_edit(&mut self, cx: &mut Context<Self>) {
        if let Some(edit) = self.pending_edit.take() {
            if let Some(runtime) = &self.runtime {
                runtime.executor.reject_edit(&edit.summary.id);
            }
            self.status = "AI edit rejected; disk was not changed".into();
            cx.emit(AgentStatus::Finished);
            cx.notify();
        }
    }

    fn persist_session(&mut self) {
        if let Some(id) = self.sessions.active_id.clone() {
            if let Some(session) = self.sessions.get_mut(&id) {
                session.messages = self.history.clone();
                if session.title == "New session" {
                    if let Some(first) = self
                        .history
                        .iter()
                        .find(|message| message.role == Role::User)
                    {
                        session.push_user(first.content.clone());
                        session.messages = self.history.clone();
                    }
                }
            }
        }
        if let Some(dir) = &self.data_dir {
            let _ = DataFiles::new(dir.clone())
                .sessions::<SessionStore>()
                .save(&self.sessions);
        }
    }

    fn toggle_plan(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.plan_mode = !self.plan_mode;
        self.plan_confirmed = false;
        self.awaiting_plan_confirmation = false;
        cx.notify();
    }

    fn confirm_plan(&mut self, cx: &mut Context<Self>) {
        if !self.awaiting_plan_confirmation || self.busy {
            return;
        }
        self.awaiting_plan_confirmation = false;
        self.plan_confirmed = true;
        self.draft.input = "Execute the approved plan now. Follow each listed step and use the normal approval workflow for every write or command.".into();
        self.cursor = self.draft.input.chars().count();
        self.submit(cx);
    }

    fn reject_plan(&mut self, cx: &mut Context<Self>) {
        self.awaiting_plan_confirmation = false;
        self.plan_confirmed = false;
        self.history.push(Message::assistant(
            "The proposed plan was rejected. No write-capable tool was exposed.",
        ));
        self.status = "Plan rejected".into();
        cx.emit(AgentStatus::Finished);
        self.persist_session();
        cx.notify();
    }

    fn send(&mut self, _event: &MouseDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.submit(cx);
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "enter" | "return" if !event.keystroke.modifiers.shift => self.submit(cx),
            "backspace" if self.cursor > 0 => {
                let start = char_to_byte(&self.draft.input, self.cursor - 1);
                let end = char_to_byte(&self.draft.input, self.cursor);
                self.draft.input.replace_range(start..end, "");
                self.cursor -= 1;
            }
            "left" => self.cursor = self.cursor.saturating_sub(1),
            "right" => self.cursor = (self.cursor + 1).min(self.draft.input.chars().count()),
            _ => return,
        }
        cx.notify();
    }

    fn display_input(&self) -> String {
        let mut input = self.draft.input.clone();
        let byte = char_to_byte(&input, self.cursor);
        input.insert_str(
            byte,
            if self.marked_text.is_empty() {
                "▏"
            } else {
                "▏…"
            },
        );
        input
    }
}

impl EventEmitter<AgentStatus> for ComposerView {}

async fn run_agent(
    runtime: AgentRuntime,
    history: Vec<Message>,
    plan_only: bool,
    event_tx: futures::channel::mpsc::UnboundedSender<termior_ai::ChatEvent>,
) -> Result<AgentOutcome, String> {
    let provider = HttpProvider::new(runtime.config).map_err(|error| error.to_string())?;
    let mut prompt = runtime.system_prompt;
    let tools = if plan_only {
        prompt.push_str("\n\nPLAN MODE: First return an explicit numbered plan with file paths and approximate ranges. Do not invoke any approval-level tool until the user confirms the plan.");
        runtime
            .tools
            .subset([
                "read_file",
                "list_directory",
                "fs_search",
                "fs_grep",
                "get_terminal_context",
            ])
            .map_err(|error| error.to_string())?
    } else {
        runtime.tools
    };
    let agent = Agent::new(Box::new(provider), tools).with_system_prompt(prompt);
    let mut on_event = move |ev: &termior_ai::ChatEvent| {
        let _ = event_tx.unbounded_send(ev.clone());
    };
    agent
        .run(
            &history,
            &|tool, arguments| {
                runtime
                    .executor
                    .execute_auto(tool, arguments)
                    .map_err(|error| error.to_string())
            },
            &mut on_event,
        )
        .await
        .map_err(|error| error.to_string())
}

async fn resume_agent(
    runtime: AgentRuntime,
    pending: PendingApproval,
    event_tx: futures::channel::mpsc::UnboundedSender<termior_ai::ChatEvent>,
) -> Result<(AgentOutcome, Option<EditProposalSummary>), String> {
    let provider = HttpProvider::new(runtime.config).map_err(|error| error.to_string())?;
    let edit = Arc::new(Mutex::new(None));
    let edit_capture = edit.clone();
    let executor = runtime.executor.clone();
    let callback = move |tool: &str, arguments: &str| {
        let output = executor
            .execute_approved(tool, arguments)
            .map_err(|error| error.to_string())?;
        if tool == "write_file" {
            if let Ok(summary) = serde_json::from_str::<EditProposalSummary>(&output) {
                *edit_capture.lock().unwrap() = Some(summary);
            }
        }
        Ok(output)
    };
    let agent =
        Agent::new(Box::new(provider), runtime.tools).with_system_prompt(runtime.system_prompt);
    let mut on_event = move |ev: &termior_ai::ChatEvent| {
        let _ = event_tx.unbounded_send(ev.clone());
    };
    let outcome = agent
        .resume(
            &pending.history,
            &callback,
            ApprovalDecision::Approve,
            &pending.request,
            &mut on_event,
        )
        .await
        .map_err(|error| error.to_string())?;
    let edit = edit.lock().unwrap().clone();
    Ok((outcome, edit))
}

impl Focusable for ComposerView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl gpui::Render for ComposerView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focus = self.focus_handle.clone();
        let input_focus = focus.clone();
        let handler = ComposerInputHandler {
            view: cx.entity().downgrade(),
        };
        let agent_label = format!("Agent: {}", self.active_agent_name());
        let messages = self
            .history
            .iter()
            .filter(|message| matches!(message.role, Role::User | Role::Assistant))
            .rev()
            .take(5)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|message| {
                let role = if message.role == Role::User {
                    "You"
                } else {
                    "Termior"
                };
                let text = message.content.lines().next().unwrap_or_default();
                div()
                    .text_xs()
                    .child(SharedString::from(format!("{role}: {text}")))
            });
        let chips = self.draft.attachments.iter().map(|attachment| {
            div()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(gpui::rgba(0x2d3748ff))
                .text_xs()
                .child(SharedString::from(attachment.chip_label()))
        });
        let approval = self.pending_approval.as_ref().map(|pending| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .mx_3()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(gpui::rgba(0xe7a93fff))
                .child(SharedString::from(format!(
                    "Approval: {}",
                    pending.request.summary
                )))
                .child(
                    div()
                        .text_xs()
                        .child(SharedString::from(pending.request.arguments.clone())),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .id("approve-tool")
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(gpui::rgba(0x3a9b62ff))
                                .cursor_pointer()
                                .child("Approve")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.approve_tool(cx)),
                                ),
                        )
                        .child(
                            div()
                                .id("reject-tool")
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(gpui::rgba(0xa64a4aff))
                                .cursor_pointer()
                                .child("Reject")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.reject_tool(cx)),
                                ),
                        ),
                )
        });
        let edit_review = self.pending_edit.as_ref().map(|edit| {
            let hunk_buttons = edit.summary.hunk_ids.iter().flat_map(|id| {
                let accept_id = *id;
                let reject_id = *id;
                let decision = edit.decisions.get(id).copied();
                [
                    div()
                        .id(SharedString::from(format!("accept-hunk-{id}")))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(if decision == Some(true) {
                            gpui::rgba(0x3a9b62ff)
                        } else {
                            gpui::rgba(0x293241ff)
                        })
                        .cursor_pointer()
                        .child(SharedString::from(format!("Accept hunk {id}")))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.decide_hunk(accept_id, true, cx)
                            }),
                        ),
                    div()
                        .id(SharedString::from(format!("reject-hunk-{id}")))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(if decision == Some(false) {
                            gpui::rgba(0xa64a4aff)
                        } else {
                            gpui::rgba(0x293241ff)
                        })
                        .cursor_pointer()
                        .child(SharedString::from(format!("Reject hunk {id}")))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.decide_hunk(reject_id, false, cx)
                            }),
                        ),
                ]
            });
            div()
                .flex()
                .flex_col()
                .gap_1()
                .mx_3()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(gpui::rgba(0x4f8fefff))
                .child(SharedString::from(format!(
                    "AI diff · {}",
                    edit.summary.path
                )))
                .child(
                    div()
                        .max_h(px(70.0))
                        .overflow_hidden()
                        .text_xs()
                        .child(SharedString::from(edit.summary.patch.clone())),
                )
                .child(div().flex().flex_wrap().gap_1().children(hunk_buttons))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .id("apply-ai-edit")
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(gpui::rgba(0x3a9b62ff))
                                .cursor_pointer()
                                .child("Apply decisions")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.apply_edit(cx)),
                                ),
                        )
                        .child(
                            div()
                                .id("reject-ai-edit")
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(gpui::rgba(0xa64a4aff))
                                .cursor_pointer()
                                .child("Reject all")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| this.reject_edit(cx)),
                                ),
                        ),
                )
        });
        let plan_review = self.awaiting_plan_confirmation.then(|| {
            div()
                .flex()
                .gap_2()
                .mx_3()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(gpui::rgba(0x4f8fefff))
                .child("Plan awaiting confirmation")
                .child(
                    div()
                        .id("confirm-plan")
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(gpui::rgba(0x3a9b62ff))
                        .cursor_pointer()
                        .child("Confirm plan")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.confirm_plan(cx)),
                        ),
                )
                .child(
                    div()
                        .id("reject-plan")
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(gpui::rgba(0xa64a4aff))
                        .cursor_pointer()
                        .child("Reject plan")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.reject_plan(cx)),
                        ),
                )
        });
        div()
            .flex()
            .flex_col()
            .relative()
            .w_full()
            .min_h(px(165.0))
            .max_h(px(330.0))
            .border_t_1()
            .border_color(gpui::rgba(0x344052ff))
            .bg(gpui::rgba(0x151a22ff))
            .track_focus(&focus)
            .on_key_down(cx.listener(Self::handle_key_down))
            .child(
                canvas(
                    |bounds, _, _| bounds,
                    move |_, _, window, cx| {
                        window.handle_input(&input_focus, handler, cx);
                    },
                )
                .absolute()
                .size_full(),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .px_3()
                    .pt_2()
                    .gap_1()
                    .children(messages),
            )
            .children(approval)
            .children(edit_review)
            .children(plan_review)
            .child(div().flex().flex_row().px_3().gap_1().children(chips))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .mx_3()
                    .mt_2()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .border_1()
                    .border_color(gpui::rgba(0x3a4658ff))
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .child(SharedString::from(self.display_input())),
                    )
                    .child(
                        div()
                            .id("composer-agent")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(gpui::rgba(0x293241ff))
                            .cursor_pointer()
                            .text_xs()
                            .child(SharedString::from(agent_label))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| this.cycle_custom_agent(cx)),
                            ),
                    )
                    .child(
                        div()
                            .id("plan-mode")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(if self.plan_mode {
                                gpui::rgba(0x4f8fefff)
                            } else {
                                gpui::rgba(0x293241ff)
                            })
                            .cursor_pointer()
                            .text_xs()
                            .child("Plan")
                            .on_mouse_down(MouseButton::Left, cx.listener(Self::toggle_plan)),
                    )
                    .child(
                        div()
                            .id("send-composer")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(if self.busy {
                                gpui::rgba(0x59606bff)
                            } else {
                                gpui::rgba(0x4f8fefff)
                            })
                            .cursor_pointer()
                            .child(if self.busy { "Working…" } else { "Send" })
                            .on_mouse_down(MouseButton::Left, cx.listener(Self::send)),
                    ),
            )
            .child(
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(gpui::rgba(0x9aa6b7ff))
                    .child(SharedString::from(self.status.clone())),
            )
    }
}

#[derive(Clone)]
struct ComposerInputHandler {
    view: WeakEntity<ComposerView>,
}

impl InputHandler for ComposerInputHandler {
    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection> {
        let view = self.view.upgrade()?;
        let view = view.read(cx);
        let pos = view
            .draft
            .input
            .chars()
            .take(view.cursor)
            .collect::<String>()
            .encode_utf16()
            .count();
        Some(UTF16Selection {
            range: pos..pos,
            reversed: false,
        })
    }
    fn marked_text_range(&mut self, _: &mut Window, cx: &mut App) -> Option<Range<usize>> {
        let view = self.view.upgrade()?;
        let len = view.read(cx).marked_text.encode_utf16().count();
        (len > 0).then_some(0..len)
    }
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<String> {
        None
    }
    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut App,
    ) {
        if let Some(view) = self.view.upgrade() {
            view.update(cx, |view, cx| {
                let byte = char_to_byte(&view.draft.input, view.cursor);
                view.draft.input.insert_str(byte, text);
                view.cursor += text.chars().count();
                view.marked_text.clear();
                cx.notify();
            });
        }
    }
    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut App,
    ) {
        if let Some(view) = self.view.upgrade() {
            view.update(cx, |view, cx| {
                view.marked_text = text.into();
                cx.notify();
            });
        }
    }
    fn unmark_text(&mut self, _: &mut Window, cx: &mut App) {
        if let Some(view) = self.view.upgrade() {
            view.update(cx, |view, cx| {
                view.marked_text.clear();
                cx.notify();
            });
        }
    }
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<Bounds<Pixels>> {
        None
    }
    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<usize> {
        None
    }
}

fn new_session_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("session-{millis}")
}

fn char_to_byte(text: &str, index: usize) -> usize {
    text.char_indices()
        .nth(index)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}
