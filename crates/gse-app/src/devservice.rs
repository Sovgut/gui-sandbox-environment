//! Installing a development build of the service, on machines that
//! `scripts/dev/install-dev-service.sh` set up. The helper copies the working
//! tree's build into place as root behind an administrator password; the app
//! then restarts the service to load it.

use std::path::Path;
use std::process::Command;

use anyhow::{Result, bail};
use gse_protocol::Request;

/// The helper the setup script installs; its polkit action asks for an
/// administrator password.
const HELPER: &str = "/usr/local/libexec/gse-dev-install";

/// Where putting a development build in place stands.
pub enum Install {
    Idle,
    Running,
    Done,
    Failed(String),
}

/// Runs the helper through pkexec, which shows the password dialog.
fn run_helper() -> Result<()> {
    let out = Command::new("pkexec").arg(HELPER).output()?;
    if out.status.success() {
        return Ok(());
    }
    // pkexec exits 126 when the dialog was dismissed or the password refused.
    if out.status.code() == Some(126) {
        bail!("not authorized");
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    bail!("{}", stderr.trim());
}

impl crate::Home {
    fn install_dev_service(&mut self, cx: &mut gpui::Context<Self>) {
        if self.preview_skips("installing the service build", cx) {
            return;
        }
        self.dev_install = Install::Running;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx.background_executor().spawn(async { run_helper() }).await;
            this.update(cx, |home, cx| {
                match outcome {
                    Ok(()) => {
                        home.dev_install = Install::Done;
                        home.restarting = true;
                        home.request("service".into(), "", Request::Restart, cx);
                    }
                    Err(e) => home.dev_install = Install::Failed(format!("{e:#}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The development card of About and updates, on a machine with the
    /// helper only.
    pub(crate) fn dev_service_card(&self, cx: &mut gpui::Context<Self>) -> Option<gpui::Div> {
        use crate::settings::Rows;
        use crate::ui::{Kind, Size, Theme, button};

        let helper = if crate::preview::active() {
            crate::preview::flag("dev")
        } else {
            Path::new(HELPER).exists()
        };
        if !helper {
            return None;
        }
        let hint = match &self.dev_install {
            Install::Idle => {
                "Puts the working tree's build in place as root and restarts the service, \
                 stopping a running sandbox and an image build; asks for the administrator \
                 password"
                    .to_owned()
            }
            Install::Running => "Waiting for the password...".to_owned(),
            Install::Done => "Installed; the service restarted with it".to_owned(),
            Install::Failed(e) => format!("Not installed: {e}"),
        };
        let tint = match self.dev_install {
            Install::Done => Theme::good(),
            Install::Failed(_) => Theme::danger(),
            Install::Idle | Install::Running => Theme::muted(),
        };
        Some(Rows::Page.group(
            Some("Development"),
            vec![Rows::Page.tinted_row(
                "Service build",
                Some(hint),
                tint,
                button(
                    "dev-install",
                    "Install service build",
                    Kind::Secondary,
                    !matches!(self.dev_install, Install::Running),
                    cx.listener(|home, _, _, cx| home.install_dev_service(cx)),
                )
                .size(Size::Small),
            )],
        ))
    }
}
