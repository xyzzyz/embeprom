use embeprom::{InstallRegistryError, Registry, Renderer};

static REGISTRY: Registry<2> = Registry::new();
static TOO_LARGE: Registry<{ embeprom::MAX_GROUPS + 1 }> = Registry::new();

embeprom::metrics! {
    struct InstalledMetrics;
    namespace = "installed";
    static METRICS;
    fn metrics;

    counter requests = "Total requests.";
}

#[test]
fn installed_registry_receives_cross_crate_lazy_registration() {
    assert_eq!(
        embeprom::install_registry(&TOO_LARGE),
        Err(InstallRegistryError::CapacityExceedsMaxGroups {
            requested: embeprom::MAX_GROUPS + 1,
            max: embeprom::MAX_GROUPS,
        })
    );
    embeprom::install_registry(&REGISTRY).unwrap();
    assert_eq!(
        embeprom::install_registry(&REGISTRY),
        Err(InstallRegistryError::AlreadyInUse)
    );

    metrics().requests.inc();
    assert_eq!(REGISTRY.len(), 1);
    assert_eq!(embeprom::group_count(), 1);

    let mut output = heapless::String::<256>::new();
    Renderer::new().render_to(&mut output).unwrap();
    assert!(output.contains("installed_requests 1\n"));
}
