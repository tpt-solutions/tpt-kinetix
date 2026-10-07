//! In-process sampling profiler (Windows x86_64 only): the admin-free stand-in
//! for `samply`. A sampler thread suspends the decoding thread every ~0.5 ms,
//! records its instruction pointer, and the report resolves each IP (through
//! inlined frames) to function names.
//!
//! Run: `KINETIX_VP9_DIR=target/perf-corpus cargo run --profile profiling -p tpt-kinetix-vp9 --example prof_sample -- [stream-substring] [iters]`
//! Prints the top self (innermost inlined) and top inclusive-by-outer-frame tables.

#[cfg(all(windows, target_arch = "x86_64"))]
mod imp {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use tpt_kinetix_vp9::Vp9Decoder;
    use tpt_kinetix_core::{packet::Packet, timestamp::Timestamp};

    #[repr(C, align(16))]
    struct Context([u8; 1232]);
    const CONTEXT_CONTROL: u32 = 0x0010_0001;
    const RIP_OFFSET: usize = 0xF8;
    const CONTEXT_FLAGS_OFFSET: usize = 0x30;
    extern "system" {
        fn GetCurrentThreadId() -> u32;
        fn OpenThread(access: u32, inherit: i32, id: u32) -> isize;
        fn SuspendThread(h: isize) -> u32;
        fn ResumeThread(h: isize) -> u32;
        fn GetThreadContext(h: isize, ctx: *mut Context) -> i32;
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> isize;
        fn Thread32First(snap: isize, e: *mut [u32; 7]) -> i32;
        fn Thread32Next(snap: isize, e: *mut [u32; 7]) -> i32;
        fn GetCurrentProcessId() -> u32;
        fn CloseHandle(h: isize) -> i32;
    }

    fn split_ivf_frames(ivf: &[u8]) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        if ivf.len() < 32 || &ivf[0..4] != b"DKIF" {
            return frames;
        }
        let mut off = 32usize;
        while off + 12 <= ivf.len() {
            let sz = u32::from_le_bytes(ivf[off..off + 4].try_into().unwrap()) as usize;
            if off + 12 + sz > ivf.len() {
                break;
            }
            frames.push(ivf[off + 12..off + 12 + sz].to_vec());
            off += 12 + sz;
        }
        frames
    }

    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    use std::sync::atomic::AtomicUsize;

    /// Counting allocator: every 32nd allocation records a short stack of raw IPs.
    pub struct Counting;
    static N_ALLOC: AtomicUsize = AtomicUsize::new(0);
    static BYTES: AtomicUsize = AtomicUsize::new(0);
    static STACKS: Mutex<Vec<([usize; 24], usize, usize)>> = Mutex::new(Vec::new());
    thread_local! { static IN: Cell<bool> = const { Cell::new(false) }; }

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let n = N_ALLOC.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(l.size(), Ordering::Relaxed);
            if n % 32 == 0 && STACKS_ON.load(Ordering::Relaxed) && !IN.with(|c| c.replace(true)) {
                let mut ips = [0usize; 24];
                let mut k = 0;
                backtrace::trace(|f| {
                    if k < 24 {
                        ips[k] = f.ip() as usize;
                        k += 1;
                    }
                    k < 24
                });
                if let Ok(mut v) = STACKS.lock() {
                    v.push((ips, k, l.size()));
                }
                IN.with(|c| c.set(false));
            }
            System.alloc(l)
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            System.dealloc(p, l)
        }
    }
    static STACKS_ON: AtomicBool = AtomicBool::new(false);

    pub fn main() {
        let dir = std::env::var("KINETIX_VP9_DIR").expect("set KINETIX_VP9_DIR (e.g. target/perf-corpus)");
        let args: Vec<String> = std::env::args().skip(1).collect();
        let pat = args.first().cloned().unwrap_or_default();
        let iters: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3);
        let mut streams = Vec::new();
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "ivf")
                && p.file_name().unwrap().to_string_lossy().contains(&pat)
            {
                streams.push(split_ivf_frames(&std::fs::read(&p).unwrap()));
            }
        }
        STACKS_ON.store(std::env::var("PROF_ALLOC").is_ok(), Ordering::Relaxed);
        let handle = unsafe { OpenThread(0x001F_FFFF, 0, GetCurrentThreadId()) };
        let stop = Arc::new(AtomicBool::new(false));
        let samples = Arc::new(Mutex::new(Vec::<usize>::new()));
        let (s2, sm2) = (stop.clone(), samples.clone());
        // PROF_ALL=1 samples every thread of the process (rayon workers
        // included), refreshing the thread list every ~20 ms.
        let all = std::env::var("PROF_ALL").is_ok();
        let t = std::thread::spawn(move || {
            let mut local = Vec::new();
            let me = unsafe { GetCurrentThreadId() };
            let mut handles: Vec<isize> = vec![handle];
            let mut last_refresh = std::time::Instant::now() - std::time::Duration::from_secs(1);
            while !s2.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_micros(500));
                if all && last_refresh.elapsed() > std::time::Duration::from_millis(20) {
                    last_refresh = std::time::Instant::now();
                    for h in handles.drain(..) {
                        if h != handle {
                            unsafe { CloseHandle(h) };
                        }
                    }
                    handles.push(handle);
                    unsafe {
                        let snap = CreateToolhelp32Snapshot(4, 0);
                        let mut e = [0u32; 7];
                        e[0] = 28;
                        let pid = GetCurrentProcessId();
                        let mut ok = Thread32First(snap, &mut e);
                        while ok != 0 {
                            if e[3] == pid && e[2] != me && e[2] != GetCurrentThreadId() {
                                let h = OpenThread(0x001F_FFFF, 0, e[2]);
                                if h != 0 {
                                    handles.push(h);
                                }
                            }
                            e[0] = 28;
                            ok = Thread32Next(snap, &mut e);
                        }
                        CloseHandle(snap);
                    }
                    // The main thread appears twice (own handle + snapshot): fine
                    // for a histogram, but drop the duplicate to keep weights even.
                    handles.truncate(handles.len());
                }
                for &h in &handles {
                    unsafe {
                        if SuspendThread(h) == u32::MAX {
                            continue;
                        }
                        let mut ctx = Context([0; 1232]);
                        ctx.0[CONTEXT_FLAGS_OFFSET..CONTEXT_FLAGS_OFFSET + 4]
                            .copy_from_slice(&CONTEXT_CONTROL.to_le_bytes());
                        if GetThreadContext(h, &mut ctx) != 0 {
                            let rip = u64::from_le_bytes(
                                ctx.0[RIP_OFFSET..RIP_OFFSET + 8].try_into().unwrap(),
                            );
                            local.push(rip as usize);
                        }
                        ResumeThread(h);
                    }
                }
            }
            sm2.lock().unwrap().extend(local);
        });
        for _ in 0..iters {
            for frames in &streams {
                let mut dec = Vp9Decoder::new();
                for (i, p) in frames.iter().enumerate() {
                    let pkt = Packet {
                        pts: Timestamp::NONE,
                        dts: Timestamp::NONE,
                        data: p.clone(),
                        stream_index: 0,
                        is_key_frame: i == 0,
                    };
                    let _ = dec.decode(&pkt);
                }
            }
        }
        stop.store(true, Ordering::Relaxed);
        STACKS_ON.store(false, Ordering::Relaxed);
        t.join().unwrap();
        println!(
            "allocations: {}  bytes: {} MB",
            N_ALLOC.load(Ordering::Relaxed),
            BYTES.load(Ordering::Relaxed) >> 20
        );
        let stacks = std::mem::take(&mut *STACKS.lock().unwrap());
        if !stacks.is_empty() {
            let mut by: HashMap<String, (usize, usize)> = HashMap::new();
            let mut cache: HashMap<usize, Option<String>> = HashMap::new();
            for (ips, k, size) in &stacks {
                let mut site = String::from("<none>");
                'o: for &ip in &ips[..*k] {
                    let e = cache.entry(ip).or_insert_with(|| {
                        let mut found = None;
                        backtrace::resolve(ip as *mut _, |s| {
                            if found.is_none() {
                                if let (Some(n), Some(f)) = (s.name(), s.lineno()) {
                                    let n = n.to_string();
                                    if n.starts_with("tpt_kinetix") {
                                        let file = s
                                            .filename()
                                            .map(|p| {
                                                p.file_name().unwrap().to_string_lossy().to_string()
                                            })
                                            .unwrap_or_default();
                                        found = Some(format!(
                                            "{} {}:{}",
                                            n.split("::h").next().unwrap(),
                                            file,
                                            f
                                        ));
                                    }
                                }
                            }
                        });
                        found
                    });
                    if let Some(x) = e {
                        site = x.clone();
                        break 'o;
                    }
                }
                let ent = by.entry(site).or_default();
                ent.0 += 1;
                ent.1 += size;
            }
            let mut v: Vec<_> = by.into_iter().collect();
            v.sort_by_key(|e| std::cmp::Reverse(e.1 .0));
            println!(
                "
== ALLOC SITES (sampled 1/32; count is x32): {} samples",
                stacks.len()
            );
            for (site, (c, b)) in v.iter().take(30) {
                println!("{:7}  avg {:7} B  {}", c * 32, b / c, site);
            }
        }
        let samples = samples.lock().unwrap();
        let total = samples.len();
        let mut uniq: HashMap<usize, usize> = HashMap::new();
        for &ip in samples.iter() {
            *uniq.entry(ip).or_default() += 1;
        }
        let mut idle = 0usize;
        let mut selfm: HashMap<String, usize> = HashMap::new();
        let mut outerm: HashMap<String, usize> = HashMap::new();
        for (&ip, &n) in &uniq {
            let mut names = Vec::new();
            backtrace::resolve(ip as *mut _, |s| {
                names.push(s.name().map_or("?".to_string(), |n| {
                    let mut x = n.to_string();
                    if let Some(i) = x.rfind("::h") {
                        x.truncate(i);
                    }
                    x
                }));
            });
            if names.is_empty() {
                names.push(format!("<unresolved {ip:#x}>"));
            }
            if std::env::var("PROF_ALL").is_ok()
                && (names[0].starts_with("Zw") || names[0].starts_with("Nt"))
            {
                idle += n;
                continue;
            }
            *selfm.entry(names[0].clone()).or_default() += n;
            *outerm.entry(names.last().unwrap().clone()).or_default() += n;
        }
        let total = total - idle;
        println!("idle (wait) samples dropped: {idle}; busy samples: {total}");
        for (title, m) in [
            ("SELF (innermost inlined frame)", selfm),
            ("OUTER (non-inlined function)", outerm),
        ] {
            let mut v: Vec<_> = m.into_iter().collect();
            v.sort_by_key(|e| std::cmp::Reverse(e.1));
            println!("\n== {title}: {total} samples");
            for (n, c) in v.iter().take(40) {
                println!("{:6.2}%  {}", *c as f64 * 100.0 / total as f64, n);
            }
        }
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
#[global_allocator]
static GLOBAL: imp::Counting = imp::Counting;

fn main() {
    #[cfg(all(windows, target_arch = "x86_64"))]
    imp::main();
    #[cfg(not(all(windows, target_arch = "x86_64")))]
    eprintln!("prof_sample is Windows x86_64 only");
}
