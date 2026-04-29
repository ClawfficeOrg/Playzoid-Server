use std::collections::HashMap;
use std::sync::Mutex;
use once_cell::sync::Lazy;
use uuid::Uuid;

// Simple in-memory ticket store for socket authentication
static TICKETS: Lazy<Mutex<HashMap<String, i64>>> = Lazy::new(|| Mutex::new(HashMap::new()));

#[allow(dead_code)]
pub fn create_ticket(alias_id: i64) -> String {
    let ticket = Uuid::new_v4().to_string();
    let mut map = TICKETS.lock().unwrap_or_else(|e| e.into_inner());
    map.insert(ticket.clone(), alias_id);
    ticket
}

pub fn verify_ticket(ticket: &str) -> Option<i64> {
    let map = TICKETS.lock().unwrap_or_else(|e| e.into_inner());
    map.get(ticket).copied()
}

#[allow(dead_code)]
pub fn revoke_ticket(ticket: &str) {
    let mut map = TICKETS.lock().unwrap_or_else(|e| e.into_inner());
    map.remove(ticket);
}
