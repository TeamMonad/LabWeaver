//! Captures only the gateway's structured events, never HTTP bodies or headers.
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tracing::{
    Event, Metadata, Subscriber,
    field::{Field, Visit},
    span::{Attributes, Id, Record},
};

#[derive(Clone, Default)]
pub struct GatewayLogs(Arc<Mutex<Vec<BTreeMap<String, String>>>>);
impl GatewayLogs {
    pub fn events(&self) -> Vec<BTreeMap<String, String>> {
        self.0.lock().expect("gateway log capture").clone()
    }
}
struct Fields(BTreeMap<String, String>);
impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_owned(), value.to_owned());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().to_owned(), format!("{value:?}"));
    }
}
impl Subscriber for GatewayLogs {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.target() == "agent_service::model_gateway"
    }
    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, event: &Event<'_>) {
        if !self.enabled(event.metadata()) {
            return;
        }
        let mut fields = Fields(BTreeMap::new());
        event.record(&mut fields);
        self.0.lock().expect("gateway log capture").push(fields.0);
    }
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}
