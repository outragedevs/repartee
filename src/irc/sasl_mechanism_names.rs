// The SASL mechanism names, strongest first — the order auto-detection walks
// and the order both wizards list.
//
// Its own file, holding nothing else, because `web-ui` is a separate crate that
// cannot depend on this binary and pulls this list in with `include!`. One
// definition rather than a mirror kept in step by hand: the two cannot drift
// because there is only one of them.
//
// Two constraints follow from being `include!`d, and both are easy to trip:
// no inner (`//!`) doc comments, which an include at item position rejects, and
// nothing here may reference the rest of this crate — web-ui has none of it.

/// Every SASL mechanism repartee implements, strongest first.
///
/// This is the flat name list. `crate::irc::SASL_MECHANISMS` is the table that
/// carries each mechanism's prerequisites and drives selection; a test binds
/// the two so neither can gain a mechanism the other lacks.
pub const SASL_MECHANISM_NAMES: &[&str] = &[
    "EXTERNAL",
    "ECDSA-NIST256P-CHALLENGE",
    "SCRAM-SHA-512",
    "SCRAM-SHA-256",
    "SCRAM-SHA-1",
    "PLAIN",
];
