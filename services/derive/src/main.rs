//! `aetherrv-derive -c /etc/aetherrv/aetherrv.toml`

fn main() -> anyhow::Result<()> {
    let args = aetherrv_core::config::Args::parse();
    aetherrv_core::logging::init("aetherrv-derive", args.verbose);
    let cfg = aetherrv_core::config::Cfg::load(&args.config)?;
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?
        .block_on(aetherrv_derive::run(cfg, aetherrv_derive::Options::default()))
}
