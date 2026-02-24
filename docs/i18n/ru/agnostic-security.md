# Локализованный bridge: Agnostic Security

Это усиленная bridge-страница. Здесь собраны позиционирование темы, навигация по разделам оригинала и практические подсказки.

Английский оригинал:

- [../../agnostic-security.md](../../agnostic-security.md)

## Позиционирование темы

- Категория: Безопасность и управление
- Глубина: усиленный bridge (карта разделов + операционные подсказки)
- Применение: сначала понять структуру, затем выполнять по английскому нормативному описанию.

## Карта разделов оригинала

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

## Практические рекомендации

- Сначала просмотрите структуру разделов оригинала, затем переходите к релевантным блокам для текущего изменения.
- Имена команд, ключей конфигурации, API-пути и code identifiers оставляйте на английском.
- При расхождениях трактовки опирайтесь на английский оригинал.

## Связанные входы

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)
