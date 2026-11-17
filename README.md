<p align="center"><img src="docs/brand/mark.svg" width="96" alt="Otaria logo"></p>

# Otaria for MariaDB

A fast desktop client built only for MariaDB: system-versioned tables, sequences, vectors, roles, Galera and more, surfaced instead of hidden.

macOS · Windows · Linux · English, Français, Español, Italiano, Deutsch

> Status: early development (phase 0: app skeleton). See [PLAN.md](PLAN.md) (French) and [docs/MARIADB_REFERENCE.md](docs/MARIADB_REFERENCE.md).

## Develop

Requirements: Node 20+, Rust (stable), and the [Tauri prerequisites](https://tauri.app/start/prerequisites/) for your OS.

```bash
npm install
npm run tauri dev      # run the app
npm run build          # i18n check + type check + frontend build
cd src-tauri && cargo test
```

Stack: Tauri 2 (Rust) · Vue 3 + TypeScript · Tailwind CSS v4 · shadcn-vue · vue-i18n.

## License

[MIT](LICENSE). Brand guidelines: [docs/brand](docs/brand/otaria-brand-guidelines.pdf).

MariaDB is a registered trademark of MariaDB Foundation / MariaDB plc. Otaria is an independent project, not affiliated with or endorsed by them.
