# AGENTS.md – LAN Bash Client

## Projektübersicht

Der Client ist ein nativer Rust-Spielclient für LAN Bash.

LAN Bash ist ein serverautoritäres Pixelart-Multiplayer-Spiel mit verschiedenen kurzen Spielmodi und vielen gleichzeitig verbundenen Spielern.

Der Client ist verantwortlich für:

- Rendering der Spielwelt
- Darstellung anderer Spieler und Objekte
- Eingabeverarbeitung
- lokale Animationen
- Audio
- Benutzeroberfläche
- Kamera
- Client-Prediction
- Interpolation
- Verbindung mit dem Server
- Darstellung serverseitig bestätigter Spielzustände

Der Client ist nicht autoritativ.

Der Server entscheidet immer über den tatsächlichen Spielzustand.

---

## Technologie

Primäre Technologien:

- Rust
- raylib
- Tokio oder eine vergleichbare Async-Runtime
- WebSocket oder QUIC für Echtzeitkommunikation
- serde
- serde_json oder ein binäres Protokoll
- tracing
- anyhow oder thiserror
- cargo fmt
- cargo clippy
- cargo test

Die tatsächlich verwendeten Versionen in `Cargo.toml` haben Vorrang.

---

## Grundprinzipien

### Der Server ist die Wahrheit

Der Client darf niemals selbstständig endgültige Entscheidungen über folgende Werte treffen:

- Spielerposition
- Geschwindigkeit
- Kollisionen
- Treffer
- Interaktionen
- Items
- Fähigkeiten
- Cooldowns
- Punkte
- Rollen
- Teams
- Spielziele
- Rundenzustand
- Sieg oder Niederlage

Der Client darf Werte vorhersagen oder darstellen. Der Serverzustand hat jedoch immer Vorrang.

### Der Client sendet Eingaben

Der Client sendet Eingaben oder Absichten an den Server.

Beispiel:

```json
{
  "type": "input",
  "client_tick": 18420,
  "sequence": 815,
  "movement": {
    "x": 1,
    "y": 0
  },
  "actions": []
}

