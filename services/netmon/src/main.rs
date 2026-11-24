//! `aetherrv-netmon -c /etc/aetherrv/aetherrv.toml`

fn main() -> anyhow::Result<()> {
    let args = aetherrv_core::config::Args::parse();
    aetherrv_core::logging::init("aetherrv-netmon", args.verbose);
    let cfg = aetherrv_core::config::Cfg::load(&args.config)?;
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?
        .block_on(aetherrv_netmon::run(cfg, aetherrv_netmon::Options { bus_socket: None }))
}
