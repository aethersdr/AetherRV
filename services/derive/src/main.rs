//! `aetherrv-derive -c /etc/aetherrv/aetherrv.toml`

fn main() -> anyhow::Result<()> {
    let args = aetherrv_core::config::Args::parse();
    aetherrv_core::logging::init("aetherrv-derive", args.verbose);
    let _cfg = aetherrv_core::config::Cfg::load(&args.config)?;
    anyhow::bail!("aetherrv-derive is not implemented yet")
}
