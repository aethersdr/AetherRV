//! `aetherrv-ruuvi -c /etc/aetherrv/aetherrv.toml`

fn main() -> anyhow::Result<()> {
    let args = aetherrv_core::config::Args::parse();
    aetherrv_core::logging::init("aetherrv-ruuvi", args.verbose);
    let cfg = aetherrv_core::config::Cfg::load(&args.config)?;
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(aetherrv_ruuvi::run(cfg, aetherrv_ruuvi::Options { bus_socket: None }))
}
