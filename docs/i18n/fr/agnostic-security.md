# Passerelle de localisation: Agnostic Security

Cette page est une passerelle enrichie. Elle fournit le positionnement du sujet, un guidage par sections source et des conseils d'exécution.

Source anglaise:

- [../../agnostic-security.md](../../agnostic-security.md)

## Positionnement du sujet

- Catégorie : Sécurité et gouvernance
- Profondeur : passerelle enrichie (guidage de sections + conseils d'exécution)
- Usage : comprendre la structure puis appliquer les étapes selon la source normative anglaise.

## Plan des sections source

- [H2 · Core Question: Will security features break...](../../agnostic-security.md#core-question-will-security-features-break)
- [H2 · 1. Build Speed: Feature-Gated Security](../../agnostic-security.md#1-build-speed-feature-gated-security)
- [H3 · Cargo.toml: Security Features Behind Features](../../agnostic-security.md#cargo-toml-security-features-behind-features)
- [H3 · Build Commands (Choose Your Profile)](../../agnostic-security.md#build-commands-choose-your-profile)
- [H3 · Conditional Compilation: Zero Overhead When Disabled](../../agnostic-security.md#conditional-compilation-zero-overhead-when-disabled)
- [H2 · 2. Pluggable Architecture: Security Is a Trait Too](../../agnostic-security.md#2-pluggable-architecture-security-is-a-trait-too)
- [H3 · Security Backend Trait (Swappable Like Everything Else)](../../agnostic-security.md#security-backend-trait-swappable-like-everything-else)
- [H3 · Factory Pattern: Auto-Select Based on Features](../../agnostic-security.md#factory-pattern-auto-select-based-on-features)
- [H2 · 3. Hardware Agnosticism: Same Binary, Different Platforms](../../agnostic-security.md#3-hardware-agnosticism-same-binary-different-platforms)
- [H3 · Cross-Platform Behavior Matrix](../../agnostic-security.md#cross-platform-behavior-matrix)
- [H3 · How It Works: Runtime Detection](../../agnostic-security.md#how-it-works-runtime-detection)
- [H2 · 4. Small Hardware: Memory Impact Analysis](../../agnostic-security.md#4-small-hardware-memory-impact-analysis)
- [H3 · Binary Size Impact (Estimated)](../../agnostic-security.md#binary-size-impact-estimated)
- [H3 · $10 Hardware Compatibility](../../agnostic-security.md#10-hardware-compatibility)
- [H2 · 5. Agnostic Swaps: Everything Remains Pluggable](../../agnostic-security.md#5-agnostic-swaps-everything-remains-pluggable)
- [H3 · ZeroClaw's Core Promise: Swap Anything](../../agnostic-security.md#zeroclaw-s-core-promise-swap-anything)
- [H3 · Swap Security Backends via Config](../../agnostic-security.md#swap-security-backends-via-config)
- [H2 · 6. Dependency Impact: Minimal New Deps](../../agnostic-security.md#6-dependency-impact-minimal-new-deps)

## Conseils d'exécution

- Commencer par la structure des sections source, puis cibler les parties directement liées au changement en cours.
- Les noms de commandes, clés de configuration, chemins API et identifiants de code restent en anglais.
- En cas d'ambiguïté d'interprétation, la source anglaise fait foi.

## Entrées liées

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)
