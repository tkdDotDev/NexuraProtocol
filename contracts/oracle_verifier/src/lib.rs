#![no_std]
use soroban_sdk::{
    contract, contractimpl, contracttype,
    Address, Env, Vec, symbol_short,
};

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Verifiers,                      // Instance: static config, set once at init
    Threshold,                      // Instance: static config, set once at init
    Verification(u64),              // Persistent: running approval count per invoice
    VerifierVoted(u64, Address),    // Persistent: anti-double-vote shield
}

// ~30 days of ledgers at ~5s/ledger. Tune to your invoice lifecycle.
const BUMP_AMOUNT: u32 = 518_400;
const LIFETIME_THRESHOLD: u32 = 500_000;

#[contract]
pub struct OracleVerifier;

#[contractimpl]
impl OracleVerifier {
    pub fn initialize(env: Env, verifiers: Vec<Address>, threshold: u32) {
        if env.storage().instance().has(&DataKey::Verifiers) {
            panic!("Already initialized");
        }
        if threshold == 0 || threshold > verifiers.len() {
            panic!("Invalid threshold");
        }
        env.storage().instance().set(&DataKey::Verifiers, &verifiers);
        env.storage().instance().set(&DataKey::Threshold, &threshold);
    }

    pub fn verify_delivery(
        env: Env,
        invoice_id: u64,
        verifier: Address,
        approved: bool,
    ) -> bool {
        verifier.require_auth();

        let verifiers: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::Verifiers)
            .expect("Not initialized");

        if !verifiers.contains(&verifier) {
            panic!("Unauthorized verifier");
        }

        // --- Anti-double-vote shield ---
        // Each verifier may cast exactly one vote per invoice. Without this,
        // a single verifier calling verify_delivery twice could single-handedly
        // push the count past the multisig threshold.
        let vote_key = DataKey::VerifierVoted(invoice_id, verifier.clone());
        if env.storage().persistent().has(&vote_key) {
            panic!("Verifier has already voted on this invoice");
        }
        env.storage().persistent().set(&vote_key, &true);
        env.storage()
            .persistent()
            .extend_ttl(&vote_key, BUMP_AMOUNT, LIFETIME_THRESHOLD);

        let count_key = DataKey::Verification(invoice_id);
        let mut count: u32 = env.storage().persistent().get(&count_key).unwrap_or(0);

        if approved {
            count = count.checked_add(1).expect("Count overflow");
            env.storage().persistent().set(&count_key, &count);
            env.storage()
                .persistent()
                .extend_ttl(&count_key, BUMP_AMOUNT, LIFETIME_THRESHOLD);
        }

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::Threshold)
            .expect("Threshold not set");

        let confirmed = count >= threshold;

        if confirmed {
            env.events().publish(
                (symbol_short!("DELIVERY"), symbol_short!("CONFIRM")),
                (invoice_id, count),
            );
        }

        confirmed
    }

    pub fn get_verification_count(env: Env, invoice_id: u64) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::Verification(invoice_id))
            .unwrap_or(0)
    }

    /// Lets a frontend check "has this specific verifier already voted"
    /// before even attempting a transaction (better UX than a failed tx).
    pub fn has_voted(env: Env, invoice_id: u64, verifier: Address) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey::VerifierVoted(invoice_id, verifier))
    }

    pub fn require_approvals(env: Env, invoice_id: u64, required_approvals: u32) -> bool {
        let count = Self::get_verification_count(env, invoice_id);
        count >= required_approvals
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::{testutils::Address as _, vec, Env};

    #[test]
    fn test_multisig_verification() {
        let env = Env::default();
        env.mock_all_auths();

        let v1 = Address::generate(&env);
        let v2 = Address::generate(&env);
        let v3 = Address::generate(&env);

        let contract_id = env.register_contract(None, OracleVerifier);
        let client = OracleVerifierClient::new(&env, &contract_id);

        client.initialize(&vec![&env, v1.clone(), v2.clone(), v3.clone()], &2);

        assert_eq!(client.verify_delivery(&1, &v1, &true), false);
        assert_eq!(client.get_verification_count(&1), 1);

        assert_eq!(client.verify_delivery(&1, &v2, &true), true);
        assert_eq!(client.get_verification_count(&1), 2);
    }

    #[test]
    #[should_panic(expected = "Verifier has already voted on this invoice")]
    fn test_double_voting_is_blocked() {
        let env = Env::default();
        env.mock_all_auths();

        let v1 = Address::generate(&env);
        let v2 = Address::generate(&env);

        let contract_id = env.register_contract(None, OracleVerifier);
        let client = OracleVerifierClient::new(&env, &contract_id);

        client.initialize(&vec![&env, v1.clone(), v2.clone()], &2);

        client.verify_delivery(&1, &v1, &true);
        // Same verifier voting again must panic instead of silently
        // pushing the count toward the threshold.
        client.verify_delivery(&1, &v1, &true);
    }

    #[test]
    fn test_require_approvals() {
        let env = Env::default();
        env.mock_all_auths();

        let v1 = Address::generate(&env);
        let v2 = Address::generate(&env);

        let contract_id = env.register_contract(None, OracleVerifier);
        let client = OracleVerifierClient::new(&env, &contract_id);

        client.initialize(&vec![&env, v1.clone(), v2.clone()], &2);

        assert_eq!(client.require_approvals(&42, &1), false);
        assert_eq!(client.verify_delivery(&42, &v1, &true), false);
        assert_eq!(client.require_approvals(&42, &1), true);
        assert_eq!(client.require_approvals(&42, &2), false);
        assert_eq!(client.verify_delivery(&42, &v2, &true), true);
        assert_eq!(client.require_approvals(&42, &2), true);
    }
}
