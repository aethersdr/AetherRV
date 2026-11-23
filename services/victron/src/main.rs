//! `aetherrv-victron -c /etc/aetherrv/aetherrv.toml`

fn main() -> anyhow::Result<()> {
    let args = aetherrv_core::config::Args::parse();
    aetherrv_core::logging::init("aetherrv-victron", args.verbose);
    let _cfg = aetherrv_core::config::Cfg::load(&args.config)?;
    anyhow::bail!("aetherrv-victron is not implemented yet")
}
