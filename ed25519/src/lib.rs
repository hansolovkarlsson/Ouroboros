//! `ed25519` — the signature primitive behind **per-machine cluster keypairs**
//! (see `docs/roadmap-cluster-keys.md` for the arc this belongs to).
//!
//! Being built in steps, each verifiable on its own, because the alternative —
//! landing a curve implementation in one reviewable-in-theory diff — is how the
//! rejected per-user-identity branch went wrong. **What is here so far:**
//!
//! - **SHA-512** (step 1). Ed25519 hashes with SHA-512 throughout: the secret
//!   key is expanded with it, the per-signature nonce is derived from it (which
//!   is what makes signing deterministic and so needs no randomness), and the
//!   challenge scalar is a hash of `R ‖ A ‖ M`.
//!
//! - **Field arithmetic mod 2²⁵⁵−19** (step 2). Five 51-bit limbs, carried on
//!   **every** operation (deliberately *not* the usual lazy reduction — see
//!   `field.rs` for why a rule the caller must remember was the wrong trade),
//!   and the inversion chain the curve layer needs.
//!
//! Still to come: curve points and scalar multiplication (step 3), sign/verify
//! (step 4, the go/no-go gate).
//!
//! ## House rules for this crate
//!
//! - **No heap, no statics that need relocating.** Any lookup table must be an
//!   array of plain integers; a table of references or slices emits
//!   `R_AARCH64_ABS64`, which this project's loader cannot process. That trap
//!   has bitten five times in other guises, and a curve implementation is the
//!   most tempting place yet to write one.
//! - **Every step is checked against an implementation that is not mine.** The
//!   test vectors below came from Python's `hashlib`, not from memory or from
//!   this code; later steps use RFC 8032's published vectors and a Python
//!   reference. A test that shares your code's assumptions confirms your bug as
//!   confidently as your correctness.

#![no_std]

mod field;
mod sha512;

pub use field::{Fe, ELEM_LEN};
pub use sha512::{sha512, Sha512, DIGEST_LEN};
