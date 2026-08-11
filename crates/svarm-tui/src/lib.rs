mod app;
mod input;
mod runtime;
mod settings;
mod theme;
mod ui;

pub use runtime::run;
pub use svarm_agent::AgentKind;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Mode {
    #[default]
    Terminal,
    Prefix,
    ChooseAgent,
    ConfirmClose,
    ConfirmQuit,
    Menu,
    Keybinds,
    Settings,
}
