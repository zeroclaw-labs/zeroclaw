<p align="center">
  <img src="../../../zeroclaw.png" alt="ZeroClaw" width="200" />
</p>

<h1 align="center">ZeroClaw 🦀</h1>

<p align="center">
  <strong>Sobrecarga cero. Compromiso cero. 100% Rust. 100% Agnóstico.</strong><br>
  ⚡️ <strong>Funciona en cualquier hardware con <5MB RAM: ¡99% menos memoria que OpenClaw y 98% más económico que un Mac mini!</strong>
</p>

<p align="center">
  <a href="../../../LICENSE-APACHE"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache%202.0-blue.svg" alt="Licencia: MIT OR Apache-2.0" /></a>
  <a href="../../../NOTICE"><img src="https://img.shields.io/github/contributors/zeroclaw-labs/zeroclaw?color=green" alt="Colaboradores" /></a>
  <a href="https://buymeacoffee.com/argenistherose"><img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-Donate-yellow.svg?style=flat&logo=buy-me-a-coffee" alt="Buy Me a Coffee" /></a>
  <a href="https://x.com/zeroclawlabs?s=21"><img src="https://img.shields.io/badge/X-%40zeroclawlabs-000000?style=flat&logo=x&logoColor=white" alt="X: @zeroclawlabs" /></a>
  <a href="https://zeroclawlabs.cn/group.jpg"><img src="https://img.shields.io/badge/WeChat-Group-B7D7A8?logo=wechat&logoColor=white" alt="Grupo WeChat" /></a>
  <a href="https://t.me/zeroclawlabs"><img src="https://img.shields.io/badge/Telegram-%40zeroclawlabs-26A5E4?style=flat&logo=telegram&logoColor=white" alt="Telegram: @zeroclawlabs" /></a>
  <a href="https://www.facebook.com/groups/zeroclaw"><img src="https://img.shields.io/badge/Facebook-Group-1877F2?style=flat&logo=facebook&logoColor=white" alt="Grupo Facebook" /></a>
  <a href="https://www.reddit.com/r/zeroclawlabs/"><img src="https://img.shields.io/badge/Reddit-r%2Fzeroclawlabs-FF4500?style=flat&logo=reddit&logoColor=white" alt="Reddit: r/zeroclawlabs" /></a>
</p>

<p align="center">
Desarrollado por estudiantes y miembros de las comunidades de Harvard, MIT y Sundai.Club.
</p>

<p align="center">
  🌐 <strong>Idiomas:</strong> <a href="../../../README.md">English</a> · <a href="../zh-CN/README.md">简体中文</a> · <a href="README.md">Español</a> · <a href="../pt/README.md">Português</a> · <a href="../it/README.md">Italiano</a> · <a href="../ja/README.md">日本語</a> · <a href="../ru/README.md">Русский</a> · <a href="../fr/README.md">Français</a> · <a href="../vi/README.md">Tiếng Việt</a> · <a href="../el/README.md">Ελληνικά</a>
</p>

<p align="center">
  <strong>Framework rápido, pequeño y totalmente autónomo</strong><br />
  Despliega en cualquier lugar. Intercambia cualquier cosa.
</p>

<p align="center">
  ZeroClaw es el <strong>framework de runtime</strong> para flujos de trabajo agents — infraestructura que abstrae modelos, herramientas, memoria y ejecución para que los agentes puedan construirse una vez y ejecutarse en cualquier lugar.
</p>

<p align="center"><code>Arquitectura basada en traits · runtime seguro por defecto · proveedor/canal/herramienta intercambiable · todo conectable</code></p>

### ✨ Características

- 🏎️ **Runtime Ligero por Defecto:** Los flujos de trabajo comunes de CLI y estado se ejecutan en una envoltura de memoria de pocos megabytes en builds de release.
- 💰 **Despliegue Económico:** Diseñado para placas de bajo costo e instancias cloud pequeñas sin dependencias de runtime pesadas.
- ⚡ **Arranques en Frío Rápidos:** El runtime Rust de binario único mantiene el inicio de comandos y daemon casi instantáneo para operaciones diarias.
- 🌍 **Arquitectura Portátil:** Un flujo de trabajo binary-first a través de ARM, x86 y RISC-V con proveedores/canales/herramientas intercambiables.
- 🔍 **Fase de Investigación:** Recopilación proactiva de información a través de herramientas antes de la generación de respuestas — reduce alucinaciones verificando hechos primero.

### Por qué los equipos eligen ZeroClaw

- **Ligero por defecto:** binario Rust pequeño, inicio rápido, huella de memoria baja.
- **Seguro por diseño:** emparejamiento, sandboxing estricto, listas de permitidos explícitas, alcance de workspace.
- **Totalmente intercambiable:** los sistemas principales son traits (proveedores, canales, herramientas, memoria, túneles).
- **Sin lock-in:** soporte de proveedor compatible con OpenAI + endpoints personalizados conectables.

## Inicio Rápido

### Opción 1: Homebrew (macOS/Linuxbrew)

```bash
brew install zeroclaw
```

### Opción 2: Clonar + Bootstrap

```bash
git clone https://github.com/zeroclaw-labs/zeroclaw.git
cd zeroclaw
./bootstrap.sh
```

> **Nota:** Las builds desde fuente requieren ~2GB RAM y ~6GB disco. Para sistemas con recursos limitados, usa `./bootstrap.sh --prefer-prebuilt` para descargar un binario pre-compilado.

### Opción 3: Cargo Install

```bash
cargo install zeroclaw
```

### Primera Ejecución

```bash
# Iniciar el gateway (sirve el API/UI del Dashboard Web)
zeroclaw gateway

# Abrir la URL del dashboard mostrada en los logs de inicio
# (por defecto: http://127.0.0.1:3000/)

# O chatear directamente
zeroclaw chat "¡Hola!"
```

Para opciones de configuración detalladas, consulta [docs/one-click-bootstrap.md](../../../docs/one-click-bootstrap.md).

---

## ⚠️ Repositorio Oficial y Advertencia de Suplantación

**Este es el único repositorio oficial de ZeroClaw:**

> https://github.com/zeroclaw-labs/zeroclaw

Cualquier otro repositorio, organización, dominio o paquete que afirme ser "ZeroClaw" o implique afiliación con ZeroClaw Labs **no está autorizado y no está afiliado con este proyecto**.

Si encuentras suplantación o uso indebido de marca, por favor [abre un issue](https://github.com/zeroclaw-labs/zeroclaw/issues).

---

## Licencia

ZeroClaw tiene doble licencia para máxima apertura y protección de colaboradores:

| Licencia | Caso de uso |
|---|---|
| [MIT](../../../LICENSE-MIT) | Open-source, investigación, académico, uso personal |
| [Apache 2.0](../../../LICENSE-APACHE) | Protección de patentes, institucional, despliegue comercial |

Puedes elegir cualquiera de las dos licencias. **Los colaboradores otorgan automáticamente derechos bajo ambas** — consulta [CLA.md](../../../CLA.md) para el acuerdo completo de colaborador.

## Contribuir

Consulta [CONTRIBUTING.md](../../../CONTRIBUTING.md) y [CLA.md](../../../CLA.md). Implementa un trait, envía un PR.

---

**ZeroClaw** — Sobrecarga cero. Compromiso cero. Despliega en cualquier lugar. Intercambia cualquier cosa. 🦀
