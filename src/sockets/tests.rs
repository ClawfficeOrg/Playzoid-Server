#[cfg(test)]
mod socket_tests {
    use crate::sockets::tickets;

    #[test]
    fn ticket_create_verify_revoke() {
        let alias = 42i64;
        let ticket = tickets::create_ticket(alias);
        assert!(!ticket.is_empty());
        let verified = tickets::verify_ticket(&ticket);
        assert_eq!(verified, Some(alias));
        tickets::revoke_ticket(&ticket);
        let after = tickets::verify_ticket(&ticket);
        assert_eq!(after, None);
    }
}
