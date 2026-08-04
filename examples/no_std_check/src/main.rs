#![no_std]
#![no_main]

use cortex_m_rt::entry;
use panic_halt as _;

embeprom::metrics! {
    pub struct DemoMetrics;
    namespace = "demo";
    static METRICS;
    fn metrics;

    counter        packets_sent            = "Total packets transmitted.";
    gauge          rssi_dbm                = "Last measured RSSI in dBm.";
    counter_vec<4> disconnects_total["reason"] = "Disconnects, by reason.";
    int_histogram  tx_latency_us[buckets: 100, 500, 1000, 5000]
                                           = "TX completion latency in microseconds.";
}

#[entry]
fn main() -> ! {
    // No explicit `embeprom::register(&METRICS)` needed: the first call to
    // `metrics()` below self-registers the group.
    metrics().packets_sent.inc();
    metrics().rssi_dbm.set(-42);
    metrics().disconnects_total.inc(&["timeout"]);
    metrics().tx_latency_us.observe(120);

    let mut renderer = embeprom::Renderer::new();
    let mut total = 0usize;
    while let Some(line) = renderer.next_line().expect("increase Renderer capacities") {
        total += line.len();
    }
    // Keep `total` alive so the render pass above isn't optimized away.
    core::hint::black_box(total);

    loop {
        cortex_m::asm::bkpt();
    }
}
