#![no_std]
#![no_main]

use cortex_m_rt::entry;
use panic_halt as _;

mod metrics {
    embeprom::metrics! {
        namespace = "demo";

        /// Total packets transmitted.
        packets_sent: Counter,
        /// Last measured RSSI in dBm.
        rssi_dbm: Gauge,
        /// Disconnects, by reason.
        #[labels("reason")]
        disconnects_total: CounterVec<4>,
        /// TX completion latency in microseconds.
        #[buckets(100, 500, 1000, 5000)]
        tx_latency_us: IntHistogram,
    }
}

#[entry]
fn main() -> ! {
    // No explicit registration needed: the first call to `metrics::get()`
    // below self-registers the group.
    metrics::get().packets_sent.inc();
    metrics::get().rssi_dbm.set(-42);
    metrics::get().disconnects_total.inc(&["timeout"]);
    metrics::get().tx_latency_us.observe(120);

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
