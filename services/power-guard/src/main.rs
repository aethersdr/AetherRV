//! `aetherrv-power-guard -c /etc/aetherrv/aetherrv.toml`

fn main() -> anyhow::Result<()> {
    let args = aetherrv_core::config::Args::parse();
    aetherrv_core::logging::init("aetherrv-power-guard", args.verbose);
    let cfg = aetherrv_core::config::Cfg::load(&args.config)?;
    tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(aetherrv_power_guard::run(cfg, None))
}
