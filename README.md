<p align="center"><img src="src/assets/mark.svg" width="96" alt="Otaria logo"></p>

# Otaria for MariaDB

A fast desktop client built only for MariaDB: system-versioned tables, sequences, vectors, roles, Galera and more, surfaced instead of hidden.

macOS · Windows · Linux · English, Français, Español, Italiano, Deutsch

> Status: early development. Downloads: [otaria-releases](https://github.com/Rhaima96/otaria-releases/releases).

**[User guide](guide/README.md)**: installation, connections (TLS, SSH), SQL editor, data editing, system-versioned tables, sequences, vectors, server administration, import and export.

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

[MIT](LICENSE).

MariaDB is a registered trademark of MariaDB Foundation / MariaDB plc. Otaria is an independent project, not affiliated with or endorsed by them.
