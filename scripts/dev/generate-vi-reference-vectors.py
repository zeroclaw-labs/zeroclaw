#!/usr/bin/env python3
"""Generate SD-JWT test vectors from the Verifiable Intent reference implementation.

The expected values in the fixture are produced by the reference at commit
356c29635f1c44df7de02edb58699ca9f29bece6, not by ZeroClaw. Regenerate with:

    python3 scripts/dev/generate-vi-reference-vectors.py /path/to/verifiable-intent \
        > crates/zeroclaw-runtime/tests/fixtures/vi-reference-vectors.json

Every field except `sd_jwt.serialized` is deterministic and re-running must produce a
byte-identical file. ECDSA draws a random nonce, so the signed SD-JWT differs on each
run while remaining valid; the script verifies what it produced rather than expecting
to reproduce a previous run. The selective-presentation vectors are computed over a
synthetic base JWT for exactly this reason — the `~` joining and hashing rules are
pure string operations and pinning them to a signature would make them unregenerable.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

REFERENCE_COMMIT = "356c29635f1c44df7de02edb58699ca9f29bece6"

# Fixed private scalar so the key material is reproducible from this script alone.
# Test-only value; it protects nothing and is never used outside these vectors.
PRIVATE_SCALAR = 0x2B7E151628AED2A6ABF7158809CF4F3C762E7160F38B4DA56A784D9045190CFE


def salt(index: int) -> str:
    """Deterministic 16-byte salt, base64url-encoded, as the spec requires."""
    from verifiable_intent.crypto.disclosure import _b64url_encode

    return _b64url_encode(bytes([index]) * 16)


def verify_reference_source(reference_root: Path, expected_commit: str) -> None:
    """Refuse to generate unless the imported source is exactly the pinned revision.

    The fixture this script writes records the reference commit, and that record
    is the reason the vectors are evidence at all rather than a ZeroClaw round
    trip. Without this check any checkout produces a file carrying the claim, so
    the claim is verified rather than asserted.

    Cleanliness is scoped to `src`, which is the directory that gets imported.
    That scope reports untracked files as well as modified ones, so a module
    dropped in to shadow an import is caught, and it ignores unrelated changes
    elsewhere in the checkout. Ignored files stay outside its view, which is what
    keeps the caches an ordinary run creates from failing the next one.

    `expected_commit` is a parameter rather than a direct read of the module
    constant so that the accompanying checks can exercise every branch against a
    throwaway repository.
    """

    def git(*args: str) -> str:
        try:
            completed = subprocess.run(
                ["git", "-C", str(reference_root), *args],
                capture_output=True,
                text=True,
                check=True,
            )
        except FileNotFoundError:
            raise SystemExit("git is required to verify the reference checkout") from None
        except subprocess.CalledProcessError as error:
            detail = error.stderr.strip() or str(error)
            raise SystemExit(
                f"{reference_root} is not a readable git checkout: {detail}"
            ) from None
        return completed.stdout.strip()

    head = git("rev-parse", "HEAD")
    if head != expected_commit:
        raise SystemExit(
            f"reference checkout is at {head}, expected {expected_commit}; "
            "vectors may only be generated from the pinned revision"
        )

    modified = git("status", "--porcelain", "--", "src")
    if modified:
        raise SystemExit(
            f"reference checkout at {expected_commit} has local changes under src:\n{modified}"
        )


def main(reference_root: Path) -> int:
    # Before anything is imported from it, so a rejected checkout is never
    # executed and the checks below need no importable reference.
    verify_reference_source(reference_root, REFERENCE_COMMIT)

    sys.path.insert(0, str(reference_root / "src"))

    from cryptography.hazmat.primitives.asymmetric import ec

    from verifiable_intent.crypto.disclosure import (
        _b64url_encode,
        build_selective_presentation,
        create_disclosure,
        hash_bytes,
        hash_disclosure,
    )
    from verifiable_intent.crypto.sd_jwt import (
        create_sd_jwt,
        decode_sd_jwt,
        resolve_disclosures,
        verify_sd_jwt_signature,
    )
    from verifiable_intent.crypto.signing import public_key_to_jwk

    private_key = ec.derive_private_key(PRIVATE_SCALAR, ec.SECP256R1())
    public_jwk = public_key_to_jwk(private_key)

    # ── Disclosures ──────────────────────────────────────────────────
    # Object-property form `[salt, name, value]` and array-element form
    # `[salt, value]`, over scalar, nested-object and array values so the
    # compact JSON separators are exercised beyond the trivial case.
    disclosure_cases = [
        ("object_scalar", salt(0), "checkout_mandate", {"vct": "x"}),
        ("object_nested", salt(1), "merchant", {"id": "m-1", "name": "Açaí & Co", "tags": ["a", "b"]}),
        ("object_number", salt(2), "amount", 1250),
        ("object_null", salt(3), "note", None),
        ("array_element", salt(4), None, {"id": "sku-1", "quantity": 2}),
        ("array_element_scalar", salt(5), None, "plain-string"),
    ]

    disclosures = []
    for case_name, case_salt, claim_name, claim_value in disclosure_cases:
        encoded = create_disclosure(claim_name, claim_value, salt=case_salt)
        disclosures.append(
            {
                "case": case_name,
                "salt": case_salt,
                "claim_name": claim_name,
                "claim_value": claim_value,
                "disclosure": encoded,
                "hash": hash_disclosure(encoded),
            }
        )

    # ── An SD-JWT exercising every resolve_disclosures branch ────────
    d_checkout = create_disclosure("checkout_mandate", {"vct": "mandate.checkout.open"}, salt=salt(16))
    d_payment = create_disclosure("payment_mandate", {"vct": "mandate.payment"}, salt=salt(17))
    d_delegate = create_disclosure(None, {"id": "agent-1", "role": "delegate"}, salt=salt(18))
    d_withheld = create_disclosure("withheld", "never-presented", salt=salt(19))
    d_array_in_sd = create_disclosure(None, "array-element-listed-in-sd", salt=salt(20))

    payload = {
        "iss": "https://issuer.example",
        "iat": 1750000000,
        "_sd_alg": "sha-256",
        # `withheld` is listed but its disclosure is not presented, so it must not resolve.
        # `d_array_in_sd` is a 2-element disclosure listed in `_sd`; the reference
        # deliberately resolves nothing for it.
        "_sd": [
            hash_disclosure(d_checkout),
            hash_disclosure(d_payment),
            hash_disclosure(d_withheld),
            hash_disclosure(d_array_in_sd),
        ],
        "delegate_payload": [
            {"...": hash_disclosure(d_delegate)},
            {"...": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"},
            {"literal": "not-a-reference"},
            "bare-string-entry",
        ],
    }
    # The VI profile defines `kb-sd-jwt` and `kb-sd-jwt+kb`; this vector is a
    # plain issuer-signed SD-JWT with no mandate pairing, so it takes the former.
    header = {"alg": "ES256", "typ": "kb-sd-jwt"}
    presented = [d_checkout, d_payment, d_delegate, d_array_in_sd]

    sd_jwt = create_sd_jwt(header, payload, presented, private_key)
    serialized = sd_jwt.serialize()

    if not verify_sd_jwt_signature(sd_jwt, private_key.public_key()):
        raise SystemExit("reference failed to verify its own SD-JWT")

    resolved = resolve_disclosures(decode_sd_jwt(serialized))

    # ── Selective presentation and its binding hash ──────────────────
    # §5.4 / §6.1.2: an L3 binds to the L2 subset it forwards, and the hash
    # covers the trailing `~` while excluding any KB-JWT. Computed over a
    # synthetic base JWT so the vector stays regenerable.
    synthetic_base = ".".join(
        [
            _b64url_encode(json.dumps({"alg": "ES256"}, separators=(",", ":")).encode()),
            _b64url_encode(
                json.dumps({"iss": "https://issuer.example"}, separators=(",", ":")).encode()
            ),
            _b64url_encode(bytes(range(64))),
        ]
    )
    subset = [d_checkout, d_delegate]
    selective = build_selective_presentation(synthetic_base, subset)

    document = {
        "_README": (
            "Expected values produced by the agent-intent/verifiable-intent reference "
            "implementation, not by ZeroClaw. See the generator named below."
        ),
        "reference_commit": REFERENCE_COMMIT,
        "generator": "scripts/dev/generate-vi-reference-vectors.py",
        "disclosures": disclosures,
        "sd_jwt": {
            "_note": (
                "`serialized` carries a real ECDSA signature and is therefore a one-time "
                "capture; every other field here is deterministic. Re-running the generator "
                "changes this one string and nothing else."
            ),
            "public_jwk": public_jwk,
            "header": header,
            "payload": payload,
            "presented_disclosures": presented,
            "serialized": serialized,
            "resolved_claims": resolved,
        },
        "selective_presentation": {
            "base_jwt": synthetic_base,
            "subset_disclosures": subset,
            "presentation": selective,
            "binding_hash": hash_bytes(selective.encode("ascii")),
        },
    }

    json.dump(document, sys.stdout, indent=2, ensure_ascii=False, sort_keys=False)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.stderr.write(f"usage: {sys.argv[0]} <path-to-verifiable-intent-checkout>\n")
        raise SystemExit(2)
    raise SystemExit(main(Path(sys.argv[1])))
