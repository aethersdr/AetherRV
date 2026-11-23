//! `aetherrv-gateway -c /etc/aetherrv/aetherrv.toml`

fn main() -> anyhow::Result<()> {
    let args = aetherrv_core::config::Args::parse();
    aetherrv_core::logging::init("aetherrv-gateway", args.verbose);
    let _cfg = aetherrv_core::config::Cfg::load(&args.config)?;
    anyhow::bail!("aetherrv-gateway is not implemented yet")
}
