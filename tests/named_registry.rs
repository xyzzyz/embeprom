use embeprom::{Registry, Renderer};

static REGISTRY: Registry<2> = Registry::new();

mod metrics {
    embeprom::metrics! {
        registry = super::REGISTRY;
        namespace = "named";

        /// Total requests.
        requests: Counter,
    }
}

#[test]
fn named_registry_receives_cross_crate_lazy_registration() {
    metrics::get().requests.inc();
    assert_eq!(REGISTRY.snapshot().len(), 1);

    let mut output = heapless::String::<256>::new();
    Renderer::<2>::from_registry(&REGISTRY)
        .render_to(&mut output)
        .unwrap();
    assert!(output.contains("named_requests 1\n"));
}
