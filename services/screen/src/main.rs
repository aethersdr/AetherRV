//! `aetherrv-screen` (started by the kiosk's sway config)

fn main() -> anyhow::Result<()> {
    aetherrv_screen::exit_with_sway();
    aetherrv_screen::wake_on_touch();
    aetherrv_screen::serve("127.0.0.1:8091")?;
    Ok(())
}
