//! Agent 选择器（Stage B 6.1）：区分内置后端、结构化外部后端和普通 CLI Agent。
//!
//! 普通 CLI Agent 不进入 Composer 的 Task/Turn 流程：选中后在当前项目的新终端里启动，
//! 只获得 PTY、附件、diff 审阅与其 hook 显式上报的通知。

use super::*;
use termior_agent_host::{CliAgentProfile, CLI_AGENTS};
use termior_theme::ResolvedPalette;

/// 选中普通 CLI Agent：请求工作区在当前项目的新终端里启动它。
#[derive(Debug, Clone, Copy)]
pub struct ComposerLaunchCliAgent(pub CliAgentProfile);

/// 菜单项对应的选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AgentMenuChoice {
    BuiltIn(Option<usize>),
    Codex,
    Cli(usize),
}

impl ComposerView {
    pub(super) fn toggle_agent_menu(&mut self, cx: &mut Context<Self>) {
        self.agent_menu_open = !self.agent_menu_open;
        if self.agent_menu_open {
            // PATH 探测只在打开菜单时做一次，避免每帧访问文件系统。
            self.cli_agent_available = CLI_AGENTS
                .iter()
                .map(|agent| agent.locate().is_some())
                .collect();
            self.mode_menu_open = false;
            self.model_menu_open = false;
        }
        cx.notify();
    }

    pub(super) fn choose_agent(&mut self, choice: AgentMenuChoice, cx: &mut Context<Self>) {
        self.agent_menu_open = false;
        match choice {
            AgentMenuChoice::BuiltIn(custom) => {
                self.select_backend(AgentBackendChoice::BuiltIn, custom, cx)
            }
            AgentMenuChoice::Codex => {
                self.select_backend(AgentBackendChoice::CodexAppServer, None, cx)
            }
            AgentMenuChoice::Cli(index) => {
                if let Some(agent) = CLI_AGENTS.get(index) {
                    self.set_status(tf!(
                        "composer.agent_menu.cli_launched",
                        "agent" => agent.display_name
                    ));
                    cx.emit(ComposerLaunchCliAgent(*agent));
                }
                cx.notify();
            }
        }
    }

    pub(super) fn agent_menu(
        &self,
        p: &ResolvedPalette,
        bottom: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected_builtin = (self.backend_choice == AgentBackendChoice::BuiltIn)
            .then_some(self.active_custom_agent);
        let mut builtin = vec![(
            AgentMenuChoice::BuiltIn(None),
            t!("composer.agent_builtin"),
            t!("composer.agent_menu.builtin_desc"),
            true,
        )];
        builtin.extend(self.custom_agents.iter().enumerate().map(|(index, agent)| {
            (
                AgentMenuChoice::BuiltIn(Some(index)),
                SharedString::from(agent.name.clone()),
                t!("composer.agent_menu.custom_desc"),
                true,
            )
        }));
        let structured = vec![(
            AgentMenuChoice::Codex,
            SharedString::from("Codex app-server"),
            t!("composer.agent_menu.codex_desc"),
            true,
        )];
        let cli = CLI_AGENTS
            .iter()
            .enumerate()
            .map(|(index, agent)| {
                let available = self
                    .cli_agent_available
                    .get(index)
                    .copied()
                    .unwrap_or(false);
                (
                    AgentMenuChoice::Cli(index),
                    SharedString::from(agent.display_name),
                    if available {
                        tf!("composer.agent_menu.cli_desc", "program" => agent.program)
                    } else {
                        tf!("composer.agent_menu.cli_missing", "program" => agent.program)
                    },
                    available,
                )
            })
            .collect::<Vec<_>>();
        let is_selected = |choice: AgentMenuChoice| match choice {
            AgentMenuChoice::BuiltIn(custom) => selected_builtin == Some(custom),
            AgentMenuChoice::Codex => self.backend_choice == AgentBackendChoice::CodexAppServer,
            AgentMenuChoice::Cli(_) => false,
        };
        let sections = [
            (t!("composer.agent_menu.section_builtin"), builtin),
            (t!("composer.agent_menu.section_structured"), structured),
            (t!("composer.agent_menu.section_cli"), cli),
        ];
        menu_panel(p)
            .id("composer-agent-menu")
            .absolute()
            .left(px(MODE_MENU_LEFT))
            .bottom(px(bottom))
            .min_w(px(260.0))
            .max_h(px(360.0))
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .children(sections.into_iter().flat_map(|(title, items)| {
                        let header = div()
                            .px_2()
                            .pt_1()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(crate::ui::muted(p))
                            .child(title)
                            .into_any_element();
                        std::iter::once(header).chain(items.into_iter().map(
                            |(choice, label, description, enabled)| {
                                let id = match choice {
                                    AgentMenuChoice::BuiltIn(None) => "builtin".to_owned(),
                                    AgentMenuChoice::BuiltIn(Some(index)) => {
                                        format!("custom-{index}")
                                    }
                                    AgentMenuChoice::Codex => "codex".to_owned(),
                                    AgentMenuChoice::Cli(index) => format!("cli-{index}"),
                                };
                                termior_ui_kit::menu_item(p, is_selected(choice), enabled)
                                    .id(SharedString::from(format!("composer-agent-{id}")))
                                    .flex()
                                    .flex_col()
                                    .gap(px(1.0))
                                    .text_xs()
                                    .child(label)
                                    .child(div().text_color(crate::ui::muted(p)).child(description))
                                    .when(enabled, |item| {
                                        item.on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.choose_agent(choice, cx);
                                            }),
                                        )
                                    })
                                    .into_any_element()
                            },
                        ))
                    })),
            )
    }
}
