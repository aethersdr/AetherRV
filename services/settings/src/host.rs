//! The Pi's own load for Settings → Device (owner, 2026-11-27): CPU, GPU and memory use and the SoC temperature,
//! read from the kernel (read-only files, no tools, no root), published as `pi.*`:
//!
//! * `pi.cpu_pct`: 100 × (1 − Δ(idle + iowait) / Δ(all)) of the `cpu` line of `/proc/stat`.
//! * `pi.gpu_pct`: the V3D GPU's `gpu_stats` (per queue: a timestamp and the queue's busy runtime, both ns): the
//!   busiest queue's Δruntime / Δtimestamp. The queues (binning, rendering, …) run at the same time, so they are not
//!   added up.
//! * `pi.mem_pct`: 100 × (1 − MemAvailable / MemTotal) of `/proc/meminfo` (what programs could not get back; the page
//!   cache doesn't count as used).
//! * `pi.soc_c`: the thermal zone of type `cpu-thermal`, the SoC's own sensor (the BCM2712 on a Pi 5), °C.
//! * `pi.rp1_c`: the RP1 I/O chip (USB, Ethernet, GPIO) on a Pi 5: hwmon `rp1_adc`, °C.
//! * `pi.nvme_c`: the NVMe SSD's own sensor: hwmon `nvme`, its "Composite" reading, °C.
//!
//! The counters are read once per publish, every `[history] interval` (30 s by default), so the CPU and GPU figures
//! are the exact means over the interval that history records them for; memory and temperature are the mean of
//! samples taken every [`SAMPLE`]. A file that isn't there (another board, an older kernel) publishes null.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How often memory and temperature are sampled between publishes.
pub const SAMPLE: Duration = Duration::from_secs(5);

/// The `cpu` line of /proc/stat: (idle + iowait, all), in jiffies.
pub fn cpu_times(stat: &str) -> Option<(u64, u64)> {
    let line = stat.lines().find(|l| l.starts_with("cpu "))?;
    let v: Vec<u64> = line.split_whitespace().skip(1).map(|x| x.parse().ok()).collect::<Option<_>>()?;
    if v.len() < 5 {
        return None;
    }
    // user nice system idle iowait irq softirq steal (guest and guest_nice are already inside user and nice)
    let all: u64 = v.iter().take(8).sum();
    Some((v[3] + v[4], all))
}

/// CPU busy % between two `cpu_times` readings.
pub fn cpu_pct(prev: (u64, u64), cur: (u64, u64)) -> Option<f64> {
    let (didle, dall) = (cur.0.checked_sub(prev.0)?, cur.1.checked_sub(prev.1)?);
    (dall > 0).then(|| (100.0 * (1.0 - didle as f64 / dall as f64)).clamp(0.0, 100.0))
}

/// Memory in use, % (1 − MemAvailable / MemTotal).
pub fn mem_pct(meminfo: &str) -> Option<f64> {
    let kb = |name: &str| -> Option<f64> {
        let l = meminfo.lines().find(|l| l.starts_with(name))?;
        l.split_whitespace().nth(1)?.parse().ok()
    };
    let (total, avail) = (kb("MemTotal:")?, kb("MemAvailable:")?);
    (total > 0.0).then(|| (100.0 * (1.0 - avail / total)).clamp(0.0, 100.0))
}

/// One GPU queue from `gpu_stats`: (name, timestamp ns, runtime ns).
pub type Queue = (String, u64, u64);

/// `gpu_stats`: a header line, then `queue timestamp jobs runtime` per queue.
pub fn gpu_queues(text: &str) -> Option<Vec<Queue>> {
    let mut out = Vec::new();
    for l in text.lines().skip(1) {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 4 {
            continue;
        }
        out.push((f[0].to_string(), f[1].parse().ok()?, f[3].parse().ok()?));
    }
    (!out.is_empty()).then_some(out)
}

/// The busiest queue's share of the time between two readings, %.
pub fn gpu_pct(prev: &[Queue], cur: &[Queue]) -> Option<f64> {
    let mut best: Option<f64> = None;
    for (name, ts, rt) in cur {
        let Some((_, pts, prt)) = prev.iter().find(|q| &q.0 == name) else { continue };
        let (dt, dr) = (ts.checked_sub(*pts)?, rt.checked_sub(*prt)?);
        if dt > 0 {
            let p = (100.0 * dr as f64 / dt as f64).clamp(0.0, 100.0);
            best = Some(best.map_or(p, |b: f64| b.max(p)));
        }
    }
    best
}

/// Where the readings come from (the real files, or test files).
#[derive(Debug, Clone)]
pub struct Paths {
    pub stat: PathBuf,
    pub meminfo: PathBuf,
    /// The `temp` file of the `cpu-thermal` zone.
    pub soc_temp: Option<PathBuf>,
    /// hwmon `rp1_adc` and hwmon `nvme` ("Composite") `temp*_input`.
    pub rp1_temp: Option<PathBuf>,
    pub nvme_temp: Option<PathBuf>,
    pub gpu_stats: Option<PathBuf>,
}

impl Paths {
    /// The running system's files; the thermal zone found by its type and `gpu_stats` by looking for the V3D device,
    /// never by a fixed index or address.
    pub fn discover() -> Paths {
        Paths {
            stat: "/proc/stat".into(),
            meminfo: "/proc/meminfo".into(),
            soc_temp: find_thermal(Path::new("/sys/class/thermal"), "cpu-thermal"),
            rp1_temp: find_hwmon_temp(Path::new("/sys/class/hwmon"), "rp1_adc", None),
            nvme_temp: find_hwmon_temp(Path::new("/sys/class/hwmon"), "nvme", Some("Composite")),
            gpu_stats: find_gpu_stats(Path::new("/sys/devices/platform")),
        }
    }
}

/// The `temp*_input` of the hwmon device named `name` (by name, never by its number, which follows the probe order):
/// the one labelled `label` if there is one, else the first.
pub fn find_hwmon_temp(dir: &Path, name: &str, label: Option<&str>) -> Option<PathBuf> {
    let mut devs: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).collect();
    devs.sort();
    let dev = devs.into_iter().find(|d| std::fs::read_to_string(d.join("name")).is_ok_and(|n| n.trim() == name))?;
    let mut temps: Vec<String> = std::fs::read_dir(&dev)
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|f| f.starts_with("temp") && f.ends_with("_input"))
        .collect();
    temps.sort();
    let labelled = |f: &String, l: &str| {
        std::fs::read_to_string(dev.join(f.replace("_input", "_label"))).is_ok_and(|x| x.trim() == l)
    };
    let pick = label.and_then(|l| temps.iter().find(|f| labelled(f, l))).or(temps.first());
    pick.map(|f| dev.join(f))
}

pub fn find_thermal(dir: &Path, kind: &str) -> Option<PathBuf> {
    let mut zones: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).collect();
    zones.sort();
    zones.into_iter().find(|z| std::fs::read_to_string(z.join("type")).is_ok_and(|t| t.trim() == kind)).map(|z| z.join("temp"))
}

/// `<platform>/**/<…>.v3d/gpu_stats`, at most two levels down (a Pi 5 has it under `axi/`).
pub fn find_gpu_stats(dir: &Path) -> Option<PathBuf> {
    let look = |d: &Path| -> Option<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(d).ok()?.flatten().map(|e| e.path()).collect();
        v.sort();
        v.into_iter().find(|p| p.extension().is_some_and(|e| e == "v3d") && p.join("gpu_stats").is_file()).map(|p| p.join("gpu_stats"))
    };
    look(dir).or_else(|| {
        let mut subs: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        subs.sort();
        subs.iter().find_map(|s| look(s))
    })
}

fn read(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok()
}

fn mean(v: &[f64]) -> Option<f64> {
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

fn r1(x: Option<f64>) -> Value {
    x.map_or(Value::Null, |x| json!(aetherrv_core::num::round(x, 1)))
}

pub struct Sampler {
    pub paths: Paths,
    cpu: Option<(u64, u64)>,
    gpu: Option<Vec<Queue>>,
    mem: Vec<f64>,
    /// SoC, RP1, NVMe samples (°C), in [`TEMPS`] order.
    temps: [Vec<f64>; 3],
}

/// The temperature keys, in the order of `Sampler::temps`.
pub const TEMPS: [&str; 3] = ["pi.soc_c", "pi.rp1_c", "pi.nvme_c"];

/// A millidegree file (`temp`, `temp1_input`) in °C.
fn milli_c(p: Option<&Path>) -> Option<f64> {
    p.and_then(read).and_then(|s| s.trim().parse::<f64>().ok()).map(|t| t / 1000.0)
}

impl Sampler {
    /// Reads the counters once, so the first publish already has a full interval behind it.
    pub fn new(paths: Paths) -> Sampler {
        let mut s = Sampler { paths, cpu: None, gpu: None, mem: vec![], temps: Default::default() };
        s.cpu = read(&s.paths.stat).as_deref().and_then(cpu_times);
        s.gpu = s.paths.gpu_stats.as_deref().and_then(read).as_deref().and_then(gpu_queues);
        s
    }

    /// Memory and temperature now (between publishes).
    pub fn sample(&mut self) {
        if let Some(m) = read(&self.paths.meminfo).as_deref().and_then(mem_pct) {
            self.mem.push(m);
        }
        let files = [self.paths.soc_temp.as_deref(), self.paths.rp1_temp.as_deref(), self.paths.nvme_temp.as_deref()];
        for (acc, f) in self.temps.iter_mut().zip(files) {
            if let Some(t) = milli_c(f) {
                acc.push(t);
            }
        }
    }

    /// The `pi.*` keys for the interval since the last publish.
    pub fn publish(&mut self) -> Vec<(String, Value)> {
        self.sample();
        let cpu = read(&self.paths.stat).as_deref().and_then(cpu_times);
        let gpu = self.paths.gpu_stats.as_deref().and_then(read).as_deref().and_then(gpu_queues);
        let cpu_v = self.cpu.zip(cpu).and_then(|(p, c)| cpu_pct(p, c));
        let gpu_v = self.gpu.as_deref().zip(gpu.as_deref()).and_then(|(p, c)| gpu_pct(p, c));
        let mut out = vec![
            ("pi.cpu_pct".to_string(), r1(cpu_v)),
            ("pi.gpu_pct".to_string(), r1(gpu_v)),
            ("pi.mem_pct".to_string(), r1(mean(&self.mem))),
        ];
        out.extend(TEMPS.iter().zip(&self.temps).map(|(k, v)| (k.to_string(), r1(mean(v)))));
        (self.cpu, self.gpu) = (cpu, gpu);
        self.mem.clear();
        self.temps.iter_mut().for_each(Vec::clear);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_from_proc_stat() {
        let a = "cpu  2540779 3159 922491 127093604 87715 0 80865 0 0 0\ncpu0 1 2 3 4 5 6 7 8 9 10\n";
        let b = "cpu  2540879 3159 922541 127094454 87715 0 80865 0 0 0\n";
        let (pa, pb) = (cpu_times(a).unwrap(), cpu_times(b).unwrap());
        assert_eq!(pa, (127_093_604 + 87_715, 2540779 + 3159 + 922491 + 127093604 + 87715 + 80865));
        // +100 user, +50 system, +850 idle: 15 % busy
        assert_eq!(cpu_pct(pa, pb).map(|x| aetherrv_core::num::round(x, 6)), Some(15.0));
        assert_eq!(cpu_pct(pa, pa), None, "no time passed");
        assert_eq!(cpu_pct(pb, pa), None, "a counter going backwards is unknown");
        assert!(cpu_times("intr 1 2 3\n").is_none());
    }

    #[test]
    fn memory_used_is_total_less_available() {
        let m = "MemTotal:       16372924 kB\nMemFree:        10780808 kB\nMemAvailable:   15268148 kB\n";
        assert_eq!(aetherrv_core::num::round(mem_pct(m).unwrap(), 2), 6.75);
        assert!(mem_pct("MemTotal: 100 kB\n").is_none());
    }

    #[test]
    fn gpu_is_the_busiest_queue_not_the_sum() {
        let a = gpu_queues("queue\ttimestamp\tjobs\truntime\nbin\t1000000000\t5\t100000000\nrender\t1000000000\t5\t200000000\ncsd\t1000000000\t0\t0\n").unwrap();
        // one second later: bin busy 0.25 s, render busy 0.40 s (they overlap)
        let b = gpu_queues("queue\ttimestamp\tjobs\truntime\nbin\t2000000000\t9\t350000000\nrender\t2000000000\t9\t600000000\ncsd\t2000000000\t0\t0\n").unwrap();
        assert_eq!(gpu_pct(&a, &b), Some(40.0));
        assert_eq!(gpu_pct(&a, &a), None);
        assert!(gpu_queues("queue\ttimestamp\tjobs\truntime\n").is_none());
    }

    #[test]
    fn sampler_on_files_and_missing_files_are_null() {
        let d = std::env::temp_dir().join(format!("aetherrv-host-{}", std::process::id()));
        std::fs::create_dir_all(d.join("thermal/thermal_zone0")).unwrap();
        std::fs::create_dir_all(d.join("thermal/thermal_zone1")).unwrap();
        std::fs::write(d.join("thermal/thermal_zone0/type"), "nvme\n").unwrap();
        std::fs::write(d.join("thermal/thermal_zone1/type"), "cpu-thermal\n").unwrap();
        std::fs::write(d.join("thermal/thermal_zone1/temp"), "39700\n").unwrap();
        std::fs::create_dir_all(d.join("platform/axi/1002000000.v3d")).unwrap();
        let gs = d.join("platform/axi/1002000000.v3d/gpu_stats");
        std::fs::write(&gs, "queue\ttimestamp\tjobs\truntime\nrender\t1000000000\t1\t0\n").unwrap();
        std::fs::write(d.join("stat"), "cpu  100 0 100 800 0 0 0 0 0 0\n").unwrap();
        std::fs::write(d.join("meminfo"), "MemTotal: 1000 kB\nMemAvailable: 750 kB\n").unwrap();
        // hwmon: the NVMe drive's "Composite" is not its first reading here; RP1 has one unlabelled reading
        for (h, name) in [("hwmon0", "cpu_thermal"), ("hwmon1", "nvme"), ("hwmon2", "rp1_adc")] {
            std::fs::create_dir_all(d.join("hwmon").join(h)).unwrap();
            std::fs::write(d.join("hwmon").join(h).join("name"), format!("{name}\n")).unwrap();
        }
        std::fs::write(d.join("hwmon/hwmon1/temp1_input"), "25000\n").unwrap();
        std::fs::write(d.join("hwmon/hwmon1/temp1_label"), "Sensor 1\n").unwrap();
        std::fs::write(d.join("hwmon/hwmon1/temp2_input"), "20850\n").unwrap();
        std::fs::write(d.join("hwmon/hwmon1/temp2_label"), "Composite\n").unwrap();
        std::fs::write(d.join("hwmon/hwmon2/temp1_input"), "40945\n").unwrap();
        let paths = Paths {
            stat: d.join("stat"),
            meminfo: d.join("meminfo"),
            soc_temp: find_thermal(&d.join("thermal"), "cpu-thermal"),
            rp1_temp: find_hwmon_temp(&d.join("hwmon"), "rp1_adc", None),
            nvme_temp: find_hwmon_temp(&d.join("hwmon"), "nvme", Some("Composite")),
            gpu_stats: find_gpu_stats(&d.join("platform")),
        };
        assert_eq!(paths.soc_temp.as_deref(), Some(d.join("thermal/thermal_zone1/temp").as_path()), "found by type, not index");
        assert_eq!(paths.gpu_stats.as_deref(), Some(gs.as_path()));
        assert_eq!(paths.nvme_temp.as_deref(), Some(d.join("hwmon/hwmon1/temp2_input").as_path()), "the Composite reading");
        assert_eq!(paths.rp1_temp.as_deref(), Some(d.join("hwmon/hwmon2/temp1_input").as_path()));
        assert!(find_hwmon_temp(&d.join("hwmon"), "nothing", None).is_none());
        let mut s = Sampler::new(paths.clone());
        std::fs::write(d.join("stat"), "cpu  150 0 150 900 0 0 0 0 0 0\n").unwrap(); // +100 busy of +200
        std::fs::write(&gs, "queue\ttimestamp\tjobs\truntime\nrender\t3000000000\t9\t500000000\n").unwrap(); // 0.5 of 2 s
        s.sample();
        std::fs::write(d.join("thermal/thermal_zone1/temp"), "41700\n").unwrap();
        let out: std::collections::HashMap<String, Value> = s.publish().into_iter().collect();
        assert_eq!((out["pi.cpu_pct"].clone(), out["pi.gpu_pct"].clone()), (json!(50.0), json!(25.0)));
        assert_eq!((out["pi.mem_pct"].clone(), out["pi.soc_c"].clone()), (json!(25.0), json!(40.7)), "the means of the samples");
        assert_eq!((out["pi.rp1_c"].clone(), out["pi.nvme_c"].clone()), (json!(40.9), json!(20.9)));
        // a board without the GPU file or the sensors: null, not an error
        let mut bare = Sampler::new(Paths { soc_temp: None, rp1_temp: None, nvme_temp: None, gpu_stats: None, ..paths });
        let out: std::collections::HashMap<String, Value> = bare.publish().into_iter().collect();
        assert_eq!((out["pi.gpu_pct"].clone(), out["pi.soc_c"].clone()), (Value::Null, Value::Null));
        assert_eq!((out["pi.rp1_c"].clone(), out["pi.nvme_c"].clone()), (Value::Null, Value::Null));
        std::fs::remove_dir_all(&d).ok();
    }
}
