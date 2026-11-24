//! `aetherrv-starlink -c /etc/aetherrv/aetherrv.toml`

fn main() -> anyhow::Result<()> {
    let args = aetherrv_core::config::Args::parse();
    aetherrv_core::logging::init("aetherrv-starlink", args.verbose);
    let cfg = aetherrv_core::config::Cfg::load(&args.config)?;
    let opts = aetherrv_starlink::Options::from_config(&cfg);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(aetherrv_starlink::run(cfg, opts))
}
