use gpui::{
    App, ClickEvent, Context, SharedString, Window, div, linear_color_stop, linear_gradient,
    prelude::*, px, rgb,
};

use crate::agent::{self, AgentSnapshot};
use crate::config::PlanckConfig;
use crate::labs::{self, Experiment, Labs, Platform, Scope, TAILNET_DISCOVERY};
use crate::tailnet::{self, Host};

const AGENT_PORT: u16 = 7337;
const BG: u32 = 0x070b10;
const SIDEBAR: u32 = 0x0a1219;
const PANEL: u32 = 0x101a24;
const PANEL_HOVER: u32 = 0x152431;
const PANEL_SOFT: u32 = 0x0d1520;
const BORDER: u32 = 0x203446;
const BORDER_SOFT: u32 = 0x182a39;
const TEXT: u32 = 0xedf5f9;
const MUTED: u32 = 0x91a6b6;
const FAINT: u32 = 0x637789;
const ACCENT: u32 = 0x38bdf8;
const ACCENT_BRIGHT: u32 = 0x7dd3fc;
const ACCENT_DEEP: u32 = 0x075985;
const INK: u32 = 0x04111c;
const GREEN: u32 = 0x34d399;
const WARNING: u32 = 0xfbbf24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Hosts,
    Agent,
    Labs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ButtonTone {
    Primary,
    Secondary,
    Ghost,
}

pub struct Planck {
    tab: Tab,
    config: PlanckConfig,
    config_path: Option<std::path::PathBuf>,
    labs: Labs,
    hosts: Vec<Host>,
    agent: Option<AgentSnapshot>,
    status: String,
    discovering: bool,
    agent_busy: bool,
    agent_snapshot_generation: usize,
}

impl Planck {
    pub fn new() -> Self {
        let (config, config_path, status) = match PlanckConfig::path() {
            Ok(path) => (
                PlanckConfig::load_or_default(&path),
                Some(path),
                "Ready".to_owned(),
            ),
            Err(error) => (
                PlanckConfig::default(),
                None,
                format!("Settings unavailable: {error:#}"),
            ),
        };
        Self {
            tab: Tab::Hosts,
            labs: Labs::new(config.labs.clone()),
            config,
            config_path,
            hosts: Vec::new(),
            agent: None,
            status,
            discovering: false,
            agent_busy: false,
            agent_snapshot_generation: 0,
        }
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.discovering {
            return;
        }
        self.discovering = true;
        self.status = "Scanning for nanoctl hosts".to_owned();
        cx.notify();
        let include_peers = self.labs.active(TAILNET_DISCOVERY);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { tailnet::discover(include_peers, AGENT_PORT) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.discovering = false;
                match result {
                    Ok(hosts) => {
                        let suffix = if include_peers {
                            "on this tailnet"
                        } else {
                            "on this device"
                        };
                        this.status = if hosts.is_empty() {
                            format!("No agents found {suffix}")
                        } else {
                            format!(
                                "{} host{} {suffix}",
                                hosts.len(),
                                if hosts.len() == 1 { "" } else { "s" }
                            )
                        };
                        this.hosts = hosts;
                    }
                    Err(error) => {
                        this.hosts.clear();
                        this.status = format!("{error:#}");
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn refresh_agent(&mut self, cx: &mut Context<Self>) {
        if self.agent_busy {
            return;
        }
        self.agent_snapshot_generation = self.agent_snapshot_generation.wrapping_add(1);
        let generation = self.agent_snapshot_generation;
        cx.spawn(async move |this, cx| {
            let snapshot = cx.background_spawn(async move { agent::inspect() }).await;
            let _ = this.update(cx, |this, cx| {
                if this.agent_snapshot_generation == generation {
                    this.agent = Some(snapshot);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn agent_action(&mut self, action: AgentAction, cx: &mut Context<Self>) {
        if self.agent_busy {
            return;
        }
        self.agent_busy = true;
        self.agent_snapshot_generation = self.agent_snapshot_generation.wrapping_add(1);
        let generation = self.agent_snapshot_generation;
        self.status = action.busy_label().to_owned();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (result, snapshot) = cx
                .background_spawn(async move { (action.run(), agent::inspect()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.agent_busy = false;
                if this.agent_snapshot_generation == generation {
                    this.agent = Some(snapshot);
                }
                this.status = match result {
                    Ok(message) => message.lines().next().unwrap_or("Done").to_owned(),
                    Err(error) => format!("{error:#}"),
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_host_experiment(&mut self, id: &'static str, cx: &mut Context<Self>) {
        let next = !self
            .agent
            .as_ref()
            .is_some_and(|snapshot| snapshot.labs.get(id).copied().unwrap_or(false));
        self.status = match agent::set_host_lab(id, next) {
            Ok(()) => {
                if let Some(snapshot) = &mut self.agent {
                    snapshot.labs.insert(id.to_owned(), next);
                }
                self.refresh_agent(cx);
                "Host experiment saved; restart the agent to apply it".to_owned()
            }
            Err(error) => format!("Could not save host experiment: {error:#}"),
        };
        cx.notify();
    }

    fn toggle_experiment(&mut self, id: &'static str, cx: &mut Context<Self>) {
        let next = !self.labs.stored(id);
        if !self.labs.set(id, next) {
            self.status = "That experiment is unavailable on this platform".to_owned();
            cx.notify();
            return;
        }
        self.config.labs = self.labs.values().clone();
        if let Some(path) = &self.config_path
            && let Err(error) = self.config.save(path)
        {
            self.status = format!("Could not save Labs settings: {error:#}");
            cx.notify();
            return;
        }
        self.status = if next {
            "Experiment enabled".to_owned()
        } else {
            "Experiment disabled".to_owned()
        };
        if id == TAILNET_DISCOVERY {
            self.refresh(cx);
        }
        cx.notify();
    }

    fn select_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        cx.notify();
    }
}

impl Render for Planck {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.tab {
            Tab::Hosts => self.hosts_tab(cx).into_any_element(),
            Tab::Agent => self.agent_tab(cx).into_any_element(),
            Tab::Labs => self.labs_tab(cx).into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .child(self.sidebar(cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.page_header(cx))
                    .child(content),
            )
    }
}

impl Planck {
    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(232.0))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .justify_between()
            .bg(rgb(SIDEBAR))
            .border_r_1()
            .border_color(rgb(BORDER_SOFT))
            .child(
                div().flex().flex_col().child(self.brand()).child(
                    div()
                        .px_3()
                        .pb_4()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(nav_item(
                            "nav-hosts",
                            "H",
                            "Hosts",
                            self.tab == Tab::Hosts,
                            cx.listener(|this, _, _, cx| this.select_tab(Tab::Hosts, cx)),
                        ))
                        .child(nav_item(
                            "nav-agent",
                            "A",
                            "Agent",
                            self.tab == Tab::Agent,
                            cx.listener(|this, _, _, cx| this.select_tab(Tab::Agent, cx)),
                        ))
                        .child(nav_item(
                            "nav-labs",
                            "L",
                            "Labs",
                            self.tab == Tab::Labs,
                            cx.listener(|this, _, _, cx| this.select_tab(Tab::Labs, cx)),
                        )),
                ),
            )
            .child(self.sidebar_status())
    }

    fn brand(&self) -> impl IntoElement {
        div()
            .p_5()
            .pb_6()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .size(px(34.0))
                    .rounded_xl()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(linear_gradient(
                        135.0,
                        linear_color_stop(rgb(ACCENT_BRIGHT), 0.0),
                        linear_color_stop(rgb(ACCENT_DEEP), 1.0),
                    ))
                    .shadow_lg()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(rgb(INK))
                            .child("n"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("nanoctl"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(FAINT))
                            .child(format!("Desktop alpha · {}", Platform::current().label())),
                    ),
            )
    }

    fn sidebar_status(&self) -> impl IntoElement {
        div()
            .m_3()
            .p_3()
            .rounded_xl()
            .bg(rgb(PANEL_SOFT))
            .border_1()
            .border_color(rgb(BORDER_SOFT))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().size(px(7.0)).rounded_full().bg(if self.discovering {
                        rgb(WARNING)
                    } else if self.hosts.is_empty() {
                        rgb(FAINT)
                    } else {
                        rgb(GREEN)
                    }))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(MUTED))
                            .child(if self.discovering {
                                "SCANNING"
                            } else {
                                "STATUS"
                            }),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(self.status.clone()),
            )
            .child(
                div()
                    .pt_1()
                    .text_xs()
                    .text_color(rgb(FAINT))
                    .child(format!("v{}", env!("CARGO_PKG_VERSION"))),
            )
    }

    fn page_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (title, detail) = match self.tab {
            Tab::Hosts => (
                "Hosts",
                if self.labs.active(TAILNET_DISCOVERY) {
                    "Choose a tailnet desktop to control."
                } else {
                    "Choose a desktop to control. Peer discovery is available in Labs."
                },
            ),
            Tab::Agent => (
                "Agent",
                "Manage this PC as a nanoctl host and its host-side experiments.",
            ),
            Tab::Labs => (
                "Labs",
                "Preview upcoming alpha features without changing stable behavior.",
            ),
        };
        div()
            .w_full()
            .px_8()
            .py_6()
            .border_b_1()
            .border_color(rgb(BORDER_SOFT))
            .flex()
            .items_end()
            .justify_between()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(div().text_sm().text_color(rgb(MUTED)).child(detail)),
            )
            .when(self.tab == Tab::Agent, |this| {
                this.child(action_button(
                    "refresh-agent",
                    if self.agent_busy {
                        "Working…"
                    } else {
                        "Refresh"
                    },
                    ButtonTone::Primary,
                    cx.listener(|this, _, _, cx| this.refresh_agent(cx)),
                ))
            })
            .when(self.tab == Tab::Hosts, |this| {
                this.child(action_button(
                    "refresh-hosts",
                    if self.discovering {
                        "Scanning…"
                    } else {
                        "Refresh"
                    },
                    ButtonTone::Primary,
                    cx.listener(|this, _, _, cx| this.refresh(cx)),
                ))
            })
    }

    fn hosts_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let hosts = self.hosts.clone();
        let rows = hosts
            .into_iter()
            .map(|host| host_row(host, cx).into_any_element())
            .collect::<Vec<_>>();
        div()
            .id("hosts-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_8()
            .py_6()
            .child(
                div()
                    .w_full()
                    .max_w(px(920.0))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .when(rows.is_empty(), |this| this.child(empty_state(cx)))
                    .children(rows),
            )
    }

    fn agent_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut children = Vec::new();
        if let Some(snapshot) = &self.agent {
            children.push(agent_status_card(snapshot).into_any_element());
            children.push(agent_actions_card(snapshot, cx).into_any_element());
            children.push(
                section_title(
                    "Host Labs",
                    "Opt into host-side alpha behavior. Changes require an agent restart.",
                )
                .into_any_element(),
            );
            for experiment in labs::EXPERIMENTS
                .iter()
                .filter(|experiment| experiment.scope == Scope::Host)
            {
                children
                    .push(host_experiment_row(experiment, &snapshot.labs, cx).into_any_element());
            }
        } else {
            children.push(
                info_card(
                    "Loading agent state",
                    "Inspecting the local nanoctl installation.",
                )
                .into_any_element(),
            );
        }
        div()
            .id("agent-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_8()
            .py_6()
            .child(
                div()
                    .w_full()
                    .max_w(px(920.0))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .children(children),
            )
    }

    fn labs_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = labs::EXPERIMENTS
            .iter()
            .filter(|experiment| experiment.scope == Scope::Client)
            .map(|experiment| experiment_row(experiment, &self.labs, cx).into_any_element())
            .collect::<Vec<_>>();
        div()
            .id("labs-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_8()
            .py_6()
            .child(
                div()
                    .w_full()
                    .max_w(px(920.0))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(labs_notice())
                    .children(rows)
                    .when_some(self.config_path.as_ref(), |this, path| {
                        this.child(
                            div()
                                .pt_3()
                                .text_xs()
                                .text_color(rgb(FAINT))
                                .child(format!("Stored in {}", path.display())),
                        )
                    }),
            )
    }
}

#[derive(Clone, Copy)]
enum AgentAction {
    Start,
    Stop,
    Restart,
}

impl AgentAction {
    fn busy_label(self) -> &'static str {
        match self {
            Self::Start => "Starting agent",
            Self::Stop => "Stopping agent",
            Self::Restart => "Restarting agent",
        }
    }

    fn run(self) -> anyhow::Result<String> {
        match self {
            Self::Start => agent::start(),
            Self::Stop => agent::stop(),
            Self::Restart => agent::restart(),
        }
    }
}

fn section_title(title: &'static str, detail: &'static str) -> impl IntoElement {
    div()
        .pt_3()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_lg()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(div().text_sm().text_color(rgb(MUTED)).child(detail))
}

fn info_card(title: &'static str, detail: &'static str) -> impl IntoElement {
    div()
        .w_full()
        .p_4()
        .rounded_xl()
        .bg(rgb(PANEL_SOFT))
        .border_1()
        .border_color(rgb(BORDER_SOFT))
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(div().text_sm().text_color(rgb(MUTED)).child(detail))
}

fn agent_status_card(snapshot: &AgentSnapshot) -> impl IntoElement {
    let state = if snapshot.running {
        "Running"
    } else if snapshot.installed {
        "Stopped"
    } else {
        "Not installed"
    };
    div()
        .w_full()
        .p_4()
        .rounded_xl()
        .bg(rgb(PANEL))
        .border_1()
        .border_color(rgb(BORDER_SOFT))
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_lg()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Local host agent"),
                        )
                        .child(
                            div().text_sm().text_color(rgb(MUTED)).child(
                                snapshot
                                    .version
                                    .clone()
                                    .unwrap_or_else(|| "nanoctl not found".into()),
                            ),
                        ),
                )
                .child(subtle_badge(
                    if snapshot.running {
                        "RUNNING"
                    } else if snapshot.installed {
                        "STOPPED"
                    } else {
                        "NOT INSTALLED"
                    },
                    if snapshot.running {
                        rgb(GREEN)
                    } else {
                        rgb(WARNING)
                    },
                )),
        )
        .child(
            div()
                .text_sm()
                .text_color(rgb(MUTED))
                .child(format!("State: {state}")),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(FAINT))
                .child(format!("Config: {}", snapshot.config_path.display())),
        )
        .when_some(snapshot.binary_path.as_ref(), |this, path| {
            this.child(
                div()
                    .text_xs()
                    .text_color(rgb(FAINT))
                    .child(format!("Binary: {}", path.display())),
            )
        })
}

fn agent_actions_card(snapshot: &AgentSnapshot, cx: &mut Context<Planck>) -> impl IntoElement {
    let config_path = snapshot.config_path.clone();
    div()
        .w_full()
        .p_4()
        .rounded_xl()
        .bg(rgb(PANEL))
        .border_1()
        .border_color(rgb(BORDER_SOFT))
        .flex()
        .flex_wrap()
        .gap_2()
        .child(action_button(
            "agent-start",
            "Start",
            ButtonTone::Primary,
            cx.listener(|this, _, _, cx| this.agent_action(AgentAction::Start, cx)),
        ))
        .child(action_button(
            "agent-stop",
            "Stop",
            ButtonTone::Secondary,
            cx.listener(|this, _, _, cx| this.agent_action(AgentAction::Stop, cx)),
        ))
        .child(action_button(
            "agent-restart",
            "Restart",
            ButtonTone::Secondary,
            cx.listener(|this, _, _, cx| this.agent_action(AgentAction::Restart, cx)),
        ))
        .child(action_button(
            "agent-open-config",
            "Open config",
            ButtonTone::Ghost,
            move |_, _, _| {
                let _ = agent::open_file(&config_path);
            },
        ))
}

fn host_experiment_row(
    experiment: &'static Experiment,
    values: &std::collections::BTreeMap<String, bool>,
    cx: &mut Context<Planck>,
) -> impl IntoElement {
    let id = experiment.id;
    let active = experiment.available_here() && values.get(id).copied().unwrap_or(false);
    div()
        .w_full()
        .p_4()
        .rounded_xl()
        .bg(rgb(PANEL))
        .border_1()
        .border_color(if active {
            rgb(ACCENT_DEEP)
        } else {
            rgb(BORDER_SOFT)
        })
        .flex()
        .items_center()
        .justify_between()
        .gap_5()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_base()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(experiment.title),
                        )
                        .child(subtle_badge("HOST EXPERIMENT", rgb(WARNING))),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .child(experiment.description),
                )
                .child(div().text_xs().text_color(rgb(FAINT)).child(format!(
                    "{} · {}",
                    experiment.id,
                    experiment.platform_label()
                ))),
        )
        .child(host_switch(id, active, cx))
}

fn host_switch(id: &'static str, active: bool, cx: &mut Context<Planck>) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("host-experiment-{id}")))
        .flex_none()
        .flex()
        .items_center()
        .w(px(46.0))
        .h(px(26.0))
        .px(px(3.0))
        .rounded_full()
        .cursor_pointer()
        .bg(if active { rgb(ACCENT) } else { rgb(BORDER) })
        .when(active, |this| this.justify_end())
        .when(!active, |this| this.justify_start())
        .child(
            div()
                .size(px(20.0))
                .rounded_full()
                .bg(rgb(TEXT))
                .shadow_md(),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.toggle_host_experiment(id, cx);
        }))
}

fn nav_item(
    id: &'static str,
    glyph: &'static str,
    label: &'static str,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(id))
        .w_full()
        .px_3()
        .py_2()
        .rounded_lg()
        .cursor_pointer()
        .flex()
        .items_center()
        .gap_3()
        .bg(if active {
            rgb(PANEL_HOVER)
        } else {
            rgb(SIDEBAR)
        })
        .border_1()
        .border_color(if active { rgb(BORDER) } else { rgb(SIDEBAR) })
        .hover(|this| this.bg(rgb(PANEL_HOVER)))
        .child(
            div()
                .size(px(24.0))
                .rounded_md()
                .flex()
                .items_center()
                .justify_center()
                .bg(if active { rgb(ACCENT_DEEP) } else { rgb(PANEL) })
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(if active {
                    rgb(ACCENT_BRIGHT)
                } else {
                    rgb(FAINT)
                })
                .child(glyph),
        )
        .child(
            div()
                .text_sm()
                .font_weight(if active {
                    gpui::FontWeight::SEMIBOLD
                } else {
                    gpui::FontWeight::NORMAL
                })
                .text_color(if active { rgb(TEXT) } else { rgb(MUTED) })
                .child(label),
        )
        .on_click(on_click)
}

fn action_button(
    id: impl Into<SharedString>,
    label: &'static str,
    tone: ButtonTone,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let (bg, hover, text, border) = match tone {
        ButtonTone::Primary => (ACCENT, ACCENT_BRIGHT, INK, ACCENT),
        ButtonTone::Secondary => (PANEL_HOVER, BORDER, TEXT, BORDER),
        ButtonTone::Ghost => (PANEL_SOFT, PANEL_HOVER, ACCENT_BRIGHT, BORDER_SOFT),
    };
    div()
        .id(id.into())
        .px_4()
        .py_2()
        .rounded_lg()
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .bg(rgb(bg))
        .text_color(rgb(text))
        .border_1()
        .border_color(rgb(border))
        .hover(move |this| this.bg(rgb(hover)))
        .active(|this| this.opacity(0.82))
        .child(label)
        .on_click(on_click)
}

fn empty_state(cx: &mut Context<Planck>) -> impl IntoElement {
    div()
        .w_full()
        .mt_6()
        .px_6()
        .py_8()
        .rounded_xl()
        .bg(rgb(PANEL_SOFT))
        .border_1()
        .border_dashed()
        .border_color(rgb(BORDER))
        .flex()
        .flex_col()
        .items_center()
        .gap_4()
        .child(
            div()
                .size(px(52.0))
                .rounded_xl()
                .flex()
                .items_center()
                .justify_center()
                .bg(linear_gradient(
                    135.0,
                    linear_color_stop(rgb(ACCENT_DEEP), 0.0),
                    linear_color_stop(rgb(PANEL_HOVER), 1.0),
                ))
                .child(
                    div()
                        .text_xl()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(rgb(ACCENT_BRIGHT))
                        .child("n"),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("No nanoctl hosts found"),
                )
                .child(
                    div()
                        .max_w(px(560.0))
                        .text_sm()
                        .text_center()
                        .text_color(rgb(MUTED))
                        .child("Install nanoctl on a host, keep Tailscale signed in, then scan again. Connect opens the agent-hosted secure viewer while the native transport is built."),
                ),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .child(action_button(
                    "empty-refresh",
                    "Scan again",
                    ButtonTone::Primary,
                    cx.listener(|this, _, _, cx| this.refresh(cx)),
                ))
                .child(action_button(
                    "empty-open-labs",
                    "Open Labs",
                    ButtonTone::Secondary,
                    cx.listener(|this, _, _, cx| this.select_tab(Tab::Labs, cx)),
                )),
        )
}

fn host_row(host: Host, _cx: &mut Context<Planck>) -> impl IntoElement {
    let secure_url = host.secure_url.clone();
    let insecure_url = host.insecure_url.clone();
    let key = if host.dns_name.is_empty() {
        host.name.clone()
    } else {
        host.dns_name.clone()
    };
    let local = host.local;
    div()
        .w_full()
        .p_4()
        .rounded_xl()
        .bg(rgb(PANEL))
        .border_1()
        .border_color(rgb(BORDER_SOFT))
        .hover(|this| this.border_color(rgb(BORDER)).bg(rgb(PANEL_HOVER)))
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .min_w_0()
                .child(
                    div()
                        .size(px(36.0))
                        .rounded_lg()
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(rgb(PANEL_HOVER))
                        .child(div().size(px(8.0)).rounded_full().bg(if local {
                            rgb(GREEN)
                        } else {
                            rgb(ACCENT)
                        })),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .min_w_0()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_base()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .child(host.name.clone()),
                                )
                                .when(local, |this| {
                                    this.child(subtle_badge("THIS DEVICE", rgb(GREEN)))
                                }),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(MUTED))
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(format!("{} · {}", host.os, host.dns_name)),
                        ),
                ),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .child(action_button(
                    format!("connect-secure-{key}"),
                    "Connect",
                    ButtonTone::Primary,
                    move |_, _, cx| cx.open_url(&secure_url),
                ))
                .child(action_button(
                    format!("connect-insecure-{key}"),
                    "HTTP",
                    ButtonTone::Ghost,
                    move |_, _, cx| cx.open_url(&insecure_url),
                )),
        )
}

fn labs_notice() -> impl IntoElement {
    div()
        .p_4()
        .rounded_xl()
        .bg(rgb(0x211a0d))
        .border_1()
        .border_color(rgb(0x6b5115))
        .flex()
        .gap_3()
        .child(
            div()
                .size(px(8.0))
                .mt_1()
                .rounded_full()
                .flex_none()
                .bg(rgb(WARNING)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Experiments are contained"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0xd6c69a))
                        .whitespace_normal()
                        .child("Each preview is off by default, stored only on this device, and can be turned off independently. Platform-specific experiments stay unavailable elsewhere."),
                ),
        )
}

fn experiment_row(
    experiment: &'static Experiment,
    labs: &Labs,
    cx: &mut Context<Planck>,
) -> impl IntoElement {
    let id = experiment.id;
    let available = experiment.available_here();
    let active = labs.active(id);
    let stored = labs.stored(id);
    div()
        .w_full()
        .p_4()
        .rounded_xl()
        .bg(rgb(PANEL))
        .border_1()
        .border_color(if active {
            rgb(ACCENT_DEEP)
        } else {
            rgb(BORDER_SOFT)
        })
        .flex()
        .items_center()
        .justify_between()
        .gap_5()
        .when(!available, |this| this.opacity(0.55))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_base()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(experiment.title),
                        )
                        .child(subtle_badge("EXPERIMENT", rgb(WARNING))),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .child(experiment.description),
                )
                .child(div().text_xs().text_color(rgb(FAINT)).child(format!(
                    "{} · {}{}",
                    experiment.id,
                    experiment.platform_label(),
                    if available {
                        ""
                    } else if stored {
                        " · stored for a supported platform"
                    } else {
                        " · unavailable here"
                    }
                ))),
        )
        .child(switch(id, active, cx))
}

fn subtle_badge(label: &'static str, color: gpui::Rgba) -> impl IntoElement {
    div()
        .px_2()
        .py_1()
        .rounded_md()
        .bg(rgb(PANEL_HOVER))
        .border_1()
        .border_color(rgb(BORDER))
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(color)
        .child(label)
}

fn switch(id: &'static str, active: bool, cx: &mut Context<Planck>) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("experiment-{id}")))
        .flex_none()
        .flex()
        .items_center()
        .w(px(46.0))
        .h(px(26.0))
        .px(px(3.0))
        .rounded_full()
        .cursor_pointer()
        .bg(if active { rgb(ACCENT) } else { rgb(BORDER) })
        .when(active, |this| this.justify_end())
        .when(!active, |this| this.justify_start())
        .shadow_sm()
        .child(
            div()
                .size(px(20.0))
                .rounded_full()
                .bg(rgb(TEXT))
                .shadow_md(),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.toggle_experiment(id, cx);
        }))
}
