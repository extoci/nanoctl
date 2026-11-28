use gpui::{
    App, ClickEvent, Context, FontWeight, SharedString, Window, div, linear_color_stop,
    linear_gradient, prelude::*, px, rgb, rgba, svg,
};

use crate::agent::{self, AgentSnapshot};
use crate::config::PlanckConfig;
use crate::labs::{self, Experiment, Labs, Scope, TAILNET_DISCOVERY};
use crate::tailnet::{self, Host};

const AGENT_PORT: u16 = 7337;
const BG: u32 = 0x081423;
const BG_DEEP: u32 = 0x020710;
const SURFACE_RAISED: u32 = 0x152335;
const SURFACE_SOFT: u32 = 0x0a141f;
const SURFACE_HOVER: u32 = 0x182a3c;
const BORDER: u32 = 0x294058;
const BORDER_SOFT: u32 = 0x182b3d;
const TEXT: u32 = 0xf2f8fc;
const MUTED: u32 = 0x9fb1c3;
const FAINT: u32 = 0x63778c;
const ACCENT: u32 = 0x22d3ee;
const ACCENT_BRIGHT: u32 = 0x7dd3fc;
const ACCENT_DEEP: u32 = 0x0e7490;
const INK: u32 = 0x031119;
const GREEN: u32 = 0x34d399;
const WARNING: u32 = 0xf59e0b;

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
            .font_family("Segoe UI")
            .text_color(rgb(TEXT))
            .bg(linear_gradient(
                135.0,
                linear_color_stop(rgb(BG), 0.0),
                linear_color_stop(rgb(BG_DEEP), 1.0),
            ))
            .flex()
            .flex_col()
            .child(self.chrome(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(self.page_header(cx))
                    .child(content),
            )
            .child(self.status_bar())
    }
}

impl Planck {
    fn chrome(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(px(72.0))
            .flex_none()
            .px_5()
            .border_b_1()
            .border_color(rgba(0x8bc6e814))
            .bg(rgba(0x030a12cc))
            .flex()
            .items_center()
            .gap_5()
            .child(self.brand())
            .child(
                div().flex_1().flex().justify_center().child(
                    div()
                        .p_1()
                        .rounded_full()
                        .bg(rgba(0x0c1621cc))
                        .border_1()
                        .border_color(rgba(0x8bc6e81f))
                        .flex()
                        .gap_1()
                        .child(top_nav_item(
                            "nav-hosts",
                            "icons/monitor.svg",
                            "Hosts",
                            self.tab == Tab::Hosts,
                            cx.listener(|this, _, _, cx| this.select_tab(Tab::Hosts, cx)),
                        ))
                        .child(top_nav_item(
                            "nav-agent",
                            "icons/agent.svg",
                            "Agent",
                            self.tab == Tab::Agent,
                            cx.listener(|this, _, _, cx| this.select_tab(Tab::Agent, cx)),
                        ))
                        .child(top_nav_item(
                            "nav-labs",
                            "icons/labs.svg",
                            "Labs",
                            self.tab == Tab::Labs,
                            cx.listener(|this, _, _, cx| this.select_tab(Tab::Labs, cx)),
                        )),
                ),
            )
            .child(self.agent_status_chip())
    }

    fn brand(&self) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .size(px(38.0))
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
                            .text_size(px(21.0))
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(INK))
                            .child("n"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("nanoctl"),
                    )
                    .child(
                        div()
                            .px_2()
                            .py(px(2.0))
                            .rounded_full()
                            .bg(rgba(0x22d3ee14))
                            .border_1()
                            .border_color(rgba(0x22d3ee30))
                            .text_xs()
                            .text_color(rgb(ACCENT_BRIGHT))
                            .child("alpha"),
                    ),
            )
    }

    fn agent_status_chip(&self) -> impl IntoElement {
        let (label, color) = match self.agent.as_ref() {
            Some(snapshot) if snapshot.running => ("Agent running", GREEN),
            Some(snapshot) if snapshot.installed => ("Agent stopped", WARNING),
            Some(_) => ("No agent", FAINT),
            None => ("Checking", FAINT),
        };
        div()
            .px_3()
            .py(px(7.0))
            .rounded_full()
            .bg(rgba(0x0c1621cc))
            .border_1()
            .border_color(rgba(0x8bc6e81f))
            .flex()
            .items_center()
            .gap_2()
            .child(div().size(px(7.0)).rounded_full().bg(rgb(color)))
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(MUTED))
                    .child(label),
            )
    }

    fn page_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (eyebrow, title, detail) = match self.tab {
            Tab::Hosts => (
                "CONNECT",
                "Remote desktops",
                if self.labs.active(TAILNET_DISCOVERY) {
                    "Choose an online tailnet machine to control."
                } else {
                    "Choose a desktop to control. Tailnet peer discovery lives in Labs."
                },
            ),
            Tab::Agent => (
                "HOST",
                "This PC",
                "Run the local agent and control host-side alpha behavior.",
            ),
            Tab::Labs => (
                "ALPHA",
                "Labs",
                "Opt into upcoming features without changing stable behavior.",
            ),
        };
        div().w_full().px_8().pt_8().pb_6().child(
            div()
                .w_full()
                .max_w(px(1080.0))
                .mx_auto()
                .flex()
                .items_end()
                .justify_between()
                .gap_6()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .min_w_0()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(ACCENT_BRIGHT))
                                .child(eyebrow),
                        )
                        .child(
                            div()
                                .text_3xl()
                                .font_weight(FontWeight::SEMIBOLD)
                                .line_height(px(34.0))
                                .child(title),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(MUTED))
                                .line_height(px(20.0))
                                .child(detail),
                        ),
                )
                .when(self.tab == Tab::Agent, |this| {
                    this.child(icon_action_button(
                        "refresh-agent",
                        "icons/refresh.svg",
                        if self.agent_busy {
                            "Working…"
                        } else {
                            "Refresh"
                        },
                        ButtonTone::Secondary,
                        cx.listener(|this, _, _, cx| this.refresh_agent(cx)),
                    ))
                })
                .when(self.tab == Tab::Hosts, |this| {
                    this.child(icon_action_button(
                        "refresh-hosts",
                        "icons/refresh.svg",
                        if self.discovering {
                            "Scanning…"
                        } else {
                            "Scan"
                        },
                        ButtonTone::Primary,
                        cx.listener(|this, _, _, cx| this.refresh(cx)),
                    ))
                }),
        )
    }

    fn hosts_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self
            .hosts
            .clone()
            .into_iter()
            .map(|host| host_card(host, cx).into_any_element())
            .collect::<Vec<_>>();
        div()
            .id("hosts-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px_8()
            .pb_6()
            .child(
                div()
                    .w_full()
                    .max_w(px(1080.0))
                    .mx_auto()
                    .h_full()
                    .when(rows.is_empty(), |this| this.child(empty_state(cx)))
                    .when(!rows.is_empty(), |this| {
                        this.child(div().grid().grid_cols(2).gap_5().children(rows))
                    }),
            )
    }

    fn agent_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut children = Vec::new();
        if let Some(snapshot) = &self.agent {
            let host_rows = labs::EXPERIMENTS
                .iter()
                .filter(|experiment| experiment.scope == Scope::Host)
                .map(|experiment| {
                    host_experiment_row(experiment, &snapshot.labs, cx).into_any_element()
                })
                .collect::<Vec<_>>();
            children.push(
                div()
                    .flex()
                    .flex_wrap()
                    .items_start()
                    .gap_5()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(430.0))
                            .flex()
                            .flex_col()
                            .gap_5()
                            .child(agent_command_card(snapshot, cx))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_3()
                                    .child(section_title(
                                        "Host Labs",
                                        "Opt into host-side alpha behavior. Changes require an agent restart.",
                                    ))
                                    .children(host_rows),
                            ),
                    )
                    .child(
                        div()
                            .w(px(350.0))
                            .flex_none()
                            .child(agent_detail_card(snapshot, cx)),
                    )
                    .into_any_element(),
            );
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
            .pb_6()
            .child(
                div()
                    .w_full()
                    .max_w(px(1080.0))
                    .mx_auto()
                    .flex()
                    .flex_col()
                    .gap_5()
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
            .pb_6()
            .child(
                div()
                    .w_full()
                    .max_w(px(1080.0))
                    .mx_auto()
                    .flex()
                    .flex_wrap()
                    .items_start()
                    .gap_5()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(560.0))
                            .flex()
                            .flex_col()
                            .gap_5()
                            .child(labs_notice())
                            .children(rows),
                    )
                    .child(labs_side_panel(self.config_path.as_ref())),
            )
    }

    fn status_bar(&self) -> impl IntoElement {
        let color = if self.discovering
            || self.agent_busy
            || self.status.contains("Could not")
            || self.status.contains("unavailable")
            || self.status.contains("not found")
        {
            WARNING
        } else {
            GREEN
        };
        div()
            .h(px(46.0))
            .flex_none()
            .px_8()
            .border_t_1()
            .border_color(rgba(0x8bc6e812))
            .bg(rgba(0x02070cb3))
            .flex()
            .items_center()
            .justify_between()
            .gap_5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .size(px(7.0))
                            .rounded_full()
                            .flex_none()
                            .bg(rgb(color)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(MUTED))
                            .truncate()
                            .child(self.status.clone()),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(FAINT))
                    .flex_none()
                    .child(format!("nanoctl desktop v{}", env!("CARGO_PKG_VERSION"))),
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

fn icon(path: &'static str, size: f32, color: u32) -> impl IntoElement {
    svg().path(path).size(px(size)).text_color(rgb(color))
}

fn icon_action_button(
    id: impl Into<SharedString>,
    icon_path: &'static str,
    label: &'static str,
    tone: ButtonTone,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let (bg, hover, text, border) = tone_colors(tone);
    div()
        .id(id.into())
        .px_4()
        .py(px(10.0))
        .rounded_xl()
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .bg(rgb(bg))
        .text_color(rgb(text))
        .border_1()
        .border_color(rgb(border))
        .hover(move |this| this.bg(rgb(hover)))
        .active(|this| this.opacity(0.82))
        .child(icon(icon_path, 16.0, text))
        .child(label)
        .on_click(on_click)
}

fn tone_colors(tone: ButtonTone) -> (u32, u32, u32, u32) {
    match tone {
        ButtonTone::Primary => (ACCENT, ACCENT_BRIGHT, INK, ACCENT),
        ButtonTone::Secondary => (SURFACE_RAISED, BORDER, TEXT, BORDER),
        ButtonTone::Ghost => (SURFACE_SOFT, SURFACE_HOVER, ACCENT_BRIGHT, BORDER_SOFT),
    }
}

fn action_button(
    id: impl Into<SharedString>,
    label: &'static str,
    tone: ButtonTone,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let (bg, hover, text, border) = tone_colors(tone);
    div()
        .id(id.into())
        .px_4()
        .py(px(10.0))
        .rounded_xl()
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .bg(rgb(bg))
        .text_color(rgb(text))
        .border_1()
        .border_color(rgb(border))
        .hover(move |this| this.bg(rgb(hover)))
        .active(|this| this.opacity(0.82))
        .child(label)
        .on_click(on_click)
}

fn top_nav_item(
    id: &'static str,
    icon_path: &'static str,
    label: &'static str,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(id))
        .min_w(px(104.0))
        .px_4()
        .py(px(8.0))
        .rounded_full()
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .bg(if active {
            rgb(ACCENT)
        } else {
            rgba(0x00000000)
        })
        .hover(|this| {
            if active {
                this.bg(rgb(ACCENT_BRIGHT))
            } else {
                this.bg(rgba(0x22d3ee14))
            }
        })
        .child(icon(icon_path, 16.0, if active { INK } else { MUTED }))
        .child(
            div()
                .text_sm()
                .font_weight(if active {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::NORMAL
                })
                .text_color(if active { rgb(INK) } else { rgb(MUTED) })
                .child(label),
        )
        .on_click(on_click)
}

fn section_title(title: &'static str, detail: &'static str) -> impl IntoElement {
    div()
        .pt_3()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_xl()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(div().text_sm().text_color(rgb(MUTED)).child(detail))
}

fn info_card(title: &'static str, detail: &'static str) -> impl IntoElement {
    div()
        .w_full()
        .p_5()
        .rounded_2xl()
        .bg(rgba(0x0c1621cc))
        .border_1()
        .border_color(rgba(0x8bc6e81f))
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(div().text_sm().text_color(rgb(MUTED)).child(detail))
}

fn state_pill(label: &'static str, color: u32) -> impl IntoElement {
    div()
        .px_3()
        .py(px(5.0))
        .rounded_full()
        .bg(rgba(0x07111ccc))
        .border_1()
        .border_color(rgb(color))
        .flex()
        .items_center()
        .gap_2()
        .child(div().size(px(7.0)).rounded_full().bg(rgb(color)))
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(TEXT))
                .child(label),
        )
}

fn agent_command_card(snapshot: &AgentSnapshot, cx: &mut Context<Planck>) -> impl IntoElement {
    let running = snapshot.running;
    let state = if running {
        "Running"
    } else if snapshot.installed {
        "Stopped"
    } else {
        "Not installed"
    };
    let detail = if running {
        "This PC is ready to accept secure nanoctl sessions."
    } else if snapshot.installed {
        "Start the agent to make this PC available on your tailnet."
    } else {
        "Install the nanoctl agent before this PC can be controlled."
    };
    div()
        .flex_1()
        .min_w(px(320.0))
        .rounded_3xl()
        .overflow_hidden()
        .bg(if running {
            linear_gradient(
                135.0,
                linear_color_stop(rgb(0x0b3441), 0.0),
                linear_color_stop(rgb(0x0a1722), 1.0),
            )
        } else {
            linear_gradient(
                135.0,
                linear_color_stop(rgb(0x182230), 0.0),
                linear_color_stop(rgb(0x0a121c), 1.0),
            )
        })
        .border_1()
        .border_color(if running {
            rgba(0x34d39955)
        } else {
            rgba(0x8bc6e81f)
        })
        .shadow_xl()
        .p_6()
        .flex()
        .flex_col()
        .gap_5()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .size(px(46.0))
                                .rounded_2xl()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(if running {
                                    rgba(0x34d3991f)
                                } else {
                                    rgba(0x22d3ee14)
                                })
                                .child(icon(
                                    "icons/power.svg",
                                    22.0,
                                    if running { GREEN } else { ACCENT_BRIGHT },
                                )),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb(MUTED))
                                        .child("LOCAL HOST AGENT"),
                                )
                                .child(
                                    div().text_sm().text_color(rgb(FAINT)).child(
                                        snapshot
                                            .version
                                            .clone()
                                            .unwrap_or_else(|| "nanoctl not found".into()),
                                    ),
                                ),
                        ),
                )
                .child(state_pill(
                    if running {
                        "RUNNING"
                    } else if snapshot.installed {
                        "STOPPED"
                    } else {
                        "MISSING"
                    },
                    if running { GREEN } else { WARNING },
                )),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_3xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(state),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .line_height(px(20.0))
                        .child(detail),
                ),
        )
        .child(
            div()
                .pt_1()
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
                    ButtonTone::Ghost,
                    cx.listener(|this, _, _, cx| this.agent_action(AgentAction::Restart, cx)),
                )),
        )
}

fn display_path(path: &std::path::Path) -> String {
    for (variable, token) in [
        ("APPDATA", "%APPDATA%"),
        ("LOCALAPPDATA", "%LOCALAPPDATA%"),
        ("ProgramFiles", "%ProgramFiles%"),
    ] {
        if let Some(root) = std::env::var_os(variable)
            && let Ok(suffix) = path.strip_prefix(std::path::Path::new(&root))
        {
            if suffix.as_os_str().is_empty() {
                return token.to_owned();
            }
            return format!("{}\\{}", token, suffix.display());
        }
    }
    path.display().to_string()
}

fn compact_middle(value: String, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars || max_chars < 8 {
        return value;
    }
    let prefix_len = (max_chars - 1) / 2;
    let suffix_len = max_chars - 1 - prefix_len;
    let prefix: String = value.chars().take(prefix_len).collect();
    let suffix: String = value
        .chars()
        .skip(count.saturating_sub(suffix_len))
        .collect();
    format!("{prefix}…{suffix}")
}

fn detail_row(label: &'static str, value: String) -> impl IntoElement {
    div()
        .flex()
        .items_start()
        .gap_3()
        .min_w_0()
        .child(
            div()
                .w(px(72.0))
                .flex_none()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(FAINT))
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(rgb(MUTED))
                .whitespace_normal()
                .line_clamp(2)
                .line_height(px(17.0))
                .child(value),
        )
}

fn agent_detail_card(snapshot: &AgentSnapshot, _cx: &mut Context<Planck>) -> impl IntoElement {
    let config_path = snapshot.config_path.clone();
    div()
        .flex_1()
        .min_w(px(320.0))
        .rounded_3xl()
        .bg(rgba(0x0c1621cc))
        .border_1()
        .border_color(rgba(0x8bc6e81f))
        .p_6()
        .flex()
        .flex_col()
        .gap_4()
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .size(px(38.0))
                        .rounded_xl()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(rgba(0x22d3ee12))
                        .child(icon("icons/folder.svg", 18.0, ACCENT_BRIGHT)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Installation"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(MUTED))
                                .child("Local paths and configuration"),
                        ),
                ),
        )
        .child(detail_row(
            "Version",
            snapshot
                .version
                .clone()
                .unwrap_or_else(|| "Unavailable".into()),
        ))
        .child(detail_row(
            "State",
            if snapshot.running {
                "Running".into()
            } else if snapshot.installed {
                "Stopped".into()
            } else {
                "Not installed".into()
            },
        ))
        .child(detail_row(
            "Config",
            compact_middle(display_path(&snapshot.config_path), 72),
        ))
        .children(snapshot.binary_path.as_ref().map(|path| {
            detail_row("Binary", compact_middle(display_path(path), 72)).into_any_element()
        }))
        .child(div().pt_2().child(icon_action_button(
            "agent-open-config",
            "icons/folder.svg",
            "Open config",
            ButtonTone::Secondary,
            move |_, _, _| {
                let _ = agent::open_file(&config_path);
            },
        )))
        .child(div().pt_1().text_xs().text_color(rgb(FAINT)).child(
            "Agent actions run hidden in the background; no helper console windows should appear.",
        ))
}

fn host_experiment_row(
    experiment: &'static Experiment,
    values: &std::collections::BTreeMap<String, bool>,
    cx: &mut Context<Planck>,
) -> impl IntoElement {
    experiment_shell(
        experiment,
        values.get(experiment.id).copied().unwrap_or(false),
        true,
        host_switch(
            experiment.id,
            values.get(experiment.id).copied().unwrap_or(false),
            cx,
        )
        .into_any_element(),
    )
}

fn experiment_shell(
    experiment: &'static Experiment,
    active: bool,
    available: bool,
    control: gpui::AnyElement,
) -> impl IntoElement {
    div()
        .p_5()
        .rounded_3xl()
        .bg(rgba(0x0c1621cc))
        .border_1()
        .border_color(if active {
            rgba(0x22d3ee66)
        } else {
            rgba(0x8bc6e81f)
        })
        .flex()
        .items_center()
        .justify_between()
        .gap_5()
        .when(!available, |this| this.opacity(0.55))
        .child(
            div()
                .flex()
                .items_center()
                .gap_4()
                .min_w_0()
                .child(
                    div()
                        .size(px(44.0))
                        .rounded_2xl()
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(if active {
                            rgba(0x22d3ee22)
                        } else {
                            rgba(0x111d2acc)
                        })
                        .child(icon(
                            "icons/labs.svg",
                            20.0,
                            if active { ACCENT_BRIGHT } else { MUTED },
                        )),
                )
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
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(experiment.title),
                                )
                                .child(subtle_badge("EXPERIMENT", rgb(WARNING))),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(MUTED))
                                .line_height(px(20.0))
                                .child(experiment.description),
                        )
                        .child(div().text_xs().text_color(rgb(FAINT)).child(format!(
                            "{} · {}",
                            experiment.id,
                            experiment.platform_label()
                        ))),
                ),
        )
        .child(control)
}

fn host_switch(id: &'static str, active: bool, cx: &mut Context<Planck>) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("host-experiment-{id}")))
        .flex_none()
        .flex()
        .items_center()
        .w(px(52.0))
        .h(px(30.0))
        .px(px(3.0))
        .rounded_full()
        .cursor_pointer()
        .bg(if active { rgb(ACCENT) } else { rgb(BORDER) })
        .when(active, |this| this.justify_end())
        .when(!active, |this| this.justify_start())
        .shadow_sm()
        .child(
            div()
                .size(px(24.0))
                .rounded_full()
                .bg(rgb(TEXT))
                .shadow_md(),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.toggle_host_experiment(id, cx);
        }))
}

fn empty_state(cx: &mut Context<Planck>) -> impl IntoElement {
    div()
        .w_full()
        .h_full()
        .min_h(px(500.0))
        .rounded_3xl()
        .overflow_hidden()
        .bg(rgba(0x0b1520dd))
        .border_1()
        .border_color(rgba(0x8bc6e81f))
        .shadow_xl()
        .flex()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .p_8()
                .flex()
                .flex_col()
                .justify_center()
                .gap_5()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(ACCENT_BRIGHT))
                                .child("NO HOSTS YET"),
                        )
                        .child(
                            div()
                                .text_3xl()
                                .font_weight(FontWeight::SEMIBOLD)
                                .line_height(px(34.0))
                                .child("Your desktops will appear here."),
                        )
                        .child(
                            div()
                                .max_w(px(460.0))
                                .text_sm()
                                .text_color(rgb(MUTED))
                                .line_height(px(21.0))
                                .child("nanoctl scans this device and, when enabled in Labs, online Tailscale peers for reachable agents. Connect opens the agent-hosted secure viewer while the native transport is built."),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(step_row("1", "Install nanoctl on the machine you want to control"))
                        .child(step_row("2", "Keep Tailscale signed in on both machines"))
                        .child(step_row("3", "Scan again and connect with HTTPS")),
                )
                .child(
                    div()
                        .pt_2()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(icon_action_button(
                            "empty-refresh",
                            "icons/refresh.svg",
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
                ),
        )
        .child(empty_art())
}

fn step_row(number: &'static str, text: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .size(px(24.0))
                .rounded_full()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x22d3ee17))
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(ACCENT_BRIGHT))
                .child(number),
        )
        .child(div().text_sm().text_color(rgb(MUTED)).child(text))
}

fn empty_art() -> impl IntoElement {
    div()
        .w(px(380.0))
        .flex_none()
        .relative()
        .overflow_hidden()
        .bg(linear_gradient(
            145.0,
            linear_color_stop(rgb(0x0c3546), 0.0),
            linear_color_stop(rgb(0x101426), 1.0),
        ))
        .child(
            div()
                .absolute()
                .top(px(42.0))
                .left(px(36.0))
                .right(px(36.0))
                .h(px(168.0))
                .rounded_2xl()
                .bg(rgba(0x07111ce6))
                .border_1()
                .border_color(rgba(0x7dd3fc3d))
                .shadow_xl()
                .flex()
                .flex_col()
                .overflow_hidden()
                .child(
                    div()
                        .h(px(30.0))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_1()
                        .border_b_1()
                        .border_color(rgba(0x8bc6e81a))
                        .child(div().size(px(7.0)).rounded_full().bg(rgb(0xfb7185)))
                        .child(div().size(px(7.0)).rounded_full().bg(rgb(WARNING)))
                        .child(div().size(px(7.0)).rounded_full().bg(rgb(GREEN))),
                )
                .child(
                    div()
                        .flex_1()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .h(px(10.0))
                                .w(px(150.0))
                                .rounded_full()
                                .bg(rgba(0x7dd3fc40)),
                        )
                        .child(
                            div()
                                .h(px(8.0))
                                .w(px(190.0))
                                .rounded_full()
                                .bg(rgba(0x91a6b626)),
                        )
                        .child(
                            div()
                                .h(px(8.0))
                                .w(px(118.0))
                                .rounded_full()
                                .bg(rgba(0x91a6b61f)),
                        )
                        .child(
                            div()
                                .mt_2()
                                .h(px(48.0))
                                .rounded_xl()
                                .bg(linear_gradient(
                                    120.0,
                                    linear_color_stop(rgb(0x0e7490), 0.0),
                                    linear_color_stop(rgb(0x1e1b4b), 1.0),
                                ))
                                .border_1()
                                .border_color(rgba(0x7dd3fc2e)),
                        ),
                ),
        )
        .child(
            div()
                .absolute()
                .bottom(px(54.0))
                .left(px(82.0))
                .right(px(82.0))
                .h(px(54.0))
                .rounded_2xl()
                .bg(rgba(0x0c1621e6))
                .border_1()
                .border_color(rgba(0x8bc6e833))
                .shadow_lg()
                .flex()
                .items_center()
                .justify_center()
                .gap_3()
                .child(icon("icons/monitor.svg", 22.0, ACCENT_BRIGHT))
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Waiting for hosts"),
                ),
        )
}

fn host_card(host: Host, _cx: &mut Context<Planck>) -> impl IntoElement {
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
        .rounded_3xl()
        .overflow_hidden()
        .bg(rgba(0x0c1621e6))
        .border_1()
        .border_color(rgba(0x8bc6e81f))
        .shadow_lg()
        .hover(|this| this.border_color(rgba(0x38bdf866)).shadow_xl())
        .flex()
        .flex_col()
        .child(host_thumbnail(local))
        .child(
            div()
                .p_5()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .items_start()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .truncate()
                                        .child(host.name.clone()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(MUTED))
                                        .truncate()
                                        .child(format!("{} · {}", host.os, host.dns_name)),
                                ),
                        )
                        .child(state_pill(
                            if local { "LOCAL" } else { "ONLINE" },
                            if local { GREEN } else { ACCENT },
                        )),
                )
                .child(
                    div()
                        .flex()
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
                ),
        )
}

fn host_thumbnail(local: bool) -> impl IntoElement {
    let accent = if local { GREEN } else { ACCENT };
    div()
        .h(px(150.0))
        .relative()
        .overflow_hidden()
        .bg(if local {
            linear_gradient(
                135.0,
                linear_color_stop(rgb(0x0b3b32), 0.0),
                linear_color_stop(rgb(0x101426), 1.0),
            )
        } else {
            linear_gradient(
                135.0,
                linear_color_stop(rgb(0x0c3546), 0.0),
                linear_color_stop(rgb(0x101426), 1.0),
            )
        })
        .child(
            div()
                .absolute()
                .top(px(18.0))
                .left(px(18.0))
                .right(px(18.0))
                .bottom(px(0.0))
                .rounded_t_2xl()
                .bg(rgba(0x06111ce6))
                .border_1()
                .border_color(rgba(0x8bc6e82e))
                .flex()
                .flex_col()
                .overflow_hidden()
                .child(
                    div()
                        .h(px(28.0))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_1()
                        .border_b_1()
                        .border_color(rgba(0x8bc6e819))
                        .child(div().size(px(6.0)).rounded_full().bg(rgb(0xfb7185)))
                        .child(div().size(px(6.0)).rounded_full().bg(rgb(WARNING)))
                        .child(div().size(px(6.0)).rounded_full().bg(rgb(GREEN))),
                )
                .child(
                    div()
                        .flex_1()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .h(px(8.0))
                                .w(px(130.0))
                                .rounded_full()
                                .bg(rgba(0x7dd3fc3d)),
                        )
                        .child(
                            div()
                                .h(px(7.0))
                                .w(px(172.0))
                                .rounded_full()
                                .bg(rgba(0x91a6b626)),
                        )
                        .child(
                            div()
                                .mt_2()
                                .flex()
                                .gap_2()
                                .child(div().h(px(42.0)).flex_1().rounded_xl().bg(rgba(0x22d3ee17)))
                                .child(
                                    div().h(px(42.0)).flex_1().rounded_xl().bg(rgba(0x8b5cf61f)),
                                ),
                        ),
                ),
        )
        .child(
            div()
                .absolute()
                .top(px(24.0))
                .right(px(28.0))
                .size(px(34.0))
                .rounded_xl()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x07111ccc))
                .border_1()
                .border_color(rgba(0x8bc6e833))
                .child(icon("icons/monitor.svg", 17.0, accent)),
        )
}

fn labs_side_panel(config_path: Option<&std::path::PathBuf>) -> impl IntoElement {
    div()
        .w(px(300.0))
        .flex_none()
        .rounded_3xl()
        .bg(rgba(0x0c1621cc))
        .border_1()
        .border_color(rgba(0x8bc6e81f))
        .p_5()
        .flex()
        .flex_col()
        .gap_4()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("How Labs works"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .child("Small safeguards keep alpha features reversible."),
                ),
        )
        .child(labs_policy_row(
            "01",
            "Local by default",
            "Experiment state is stored only on this device.",
        ))
        .child(labs_policy_row(
            "02",
            "Platform gated",
            "Windows-only previews stay unavailable elsewhere.",
        ))
        .child(labs_policy_row(
            "03",
            "Independently reversible",
            "Each experiment can be turned off without affecting the others.",
        ))
        .when_some(config_path, |this, path| {
            this.child(
                div()
                    .pt_2()
                    .border_t_1()
                    .border_color(rgba(0x8bc6e814))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(FAINT))
                            .child("STORED AT"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .whitespace_normal()
                            .line_clamp(2)
                            .line_height(px(18.0))
                            .child(compact_middle(display_path(path), 64)),
                    ),
            )
        })
}

fn labs_policy_row(
    number: &'static str,
    title: &'static str,
    detail: &'static str,
) -> impl IntoElement {
    div()
        .flex()
        .gap_3()
        .child(
            div()
                .size(px(28.0))
                .rounded_full()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x22d3ee14))
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(ACCENT_BRIGHT))
                .child(number),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .line_height(px(18.0))
                        .child(detail),
                ),
        )
}

fn labs_notice() -> impl IntoElement {
    div()
        .p_5()
        .rounded_3xl()
        .overflow_hidden()
        .bg(linear_gradient(
            120.0,
            linear_color_stop(rgb(0x123144), 0.0),
            linear_color_stop(rgb(0x181634), 1.0),
        ))
        .border_1()
        .border_color(rgba(0x7dd3fc33))
        .flex()
        .items_center()
        .gap_4()
        .child(
            div()
                .size(px(48.0))
                .rounded_2xl()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x22d3ee1f))
                .child(icon("icons/labs.svg", 23.0, ACCENT_BRIGHT)),
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
                        .text_lg()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Controlled alpha surface"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .line_height(px(20.0))
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
    div()
        .w_full()
        .p_5()
        .rounded_3xl()
        .bg(rgba(0x0c1621cc))
        .border_1()
        .border_color(if active {
            rgba(0x22d3ee66)
        } else {
            rgba(0x8bc6e81f)
        })
        .flex()
        .items_center()
        .justify_between()
        .gap_5()
        .when(!available, |this| this.opacity(0.55))
        .child(
            div()
                .flex()
                .items_center()
                .gap_4()
                .min_w_0()
                .child(
                    div()
                        .size(px(44.0))
                        .rounded_2xl()
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(if active {
                            rgba(0x22d3ee22)
                        } else {
                            rgba(0x111d2acc)
                        })
                        .child(icon(
                            "icons/labs.svg",
                            20.0,
                            if active { ACCENT_BRIGHT } else { MUTED },
                        )),
                )
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
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(experiment.title),
                                )
                                .child(subtle_badge("EXPERIMENT", rgb(WARNING))),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(MUTED))
                                .line_height(px(20.0))
                                .child(experiment.description),
                        )
                        .child(div().text_xs().text_color(rgb(FAINT)).child(format!(
                            "{} · {}{}",
                            experiment.id,
                            experiment.platform_label(),
                            if available {
                                ""
                            } else if labs.stored(id) {
                                " · stored for a supported platform"
                            } else {
                                " · unavailable here"
                            }
                        ))),
                ),
        )
        .child(switch(id, active, cx))
}

fn subtle_badge(label: &'static str, color: gpui::Rgba) -> impl IntoElement {
    div()
        .px_2()
        .py(px(3.0))
        .rounded_full()
        .bg(rgba(0x111d2acc))
        .border_1()
        .border_color(rgba(0x8bc6e82e))
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(color)
        .child(label)
}

fn switch(id: &'static str, active: bool, cx: &mut Context<Planck>) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("experiment-{id}")))
        .flex_none()
        .flex()
        .items_center()
        .w(px(52.0))
        .h(px(30.0))
        .px(px(3.0))
        .rounded_full()
        .cursor_pointer()
        .bg(if active { rgb(ACCENT) } else { rgb(BORDER) })
        .when(active, |this| this.justify_end())
        .when(!active, |this| this.justify_start())
        .shadow_sm()
        .child(
            div()
                .size(px(24.0))
                .rounded_full()
                .bg(rgb(TEXT))
                .shadow_md(),
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.toggle_experiment(id, cx);
        }))
}
