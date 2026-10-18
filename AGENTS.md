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

### Async-Runtime

Die Verbindung wird von `net::session` in einem eigenen Thread mit eigener
Tokio-Runtime aufgebaut. Der Future für den eigentlichen Transport muss
allerdings *in* einer Runtime erzeugt und gepollt werden.

- Der Game-Thread (`ClientApp`) besitzt dafür seine eigene Runtime, wenn der
  Aufrufer keine mitbringt. Tests müssen `build_tokio_runtime()` benutzen und
  `enter()`en.
- Eine Runtime ohne Reactor führt nicht zu einem Fehler, sondern dazu, dass der
  Future nie gepollt wird ("verbunden", aber null Snapshots).
- Eine Multi-Thread-Runtime darf nicht innerhalb von `block_on` gedroppt
  werden (`cannot drop a runtime where blocking is not allowed`).
  Current-Thread-Runtimes haben dieses Problem nicht.
- `run_headless` blockiert den Game-Thread: dort muss die Runtime auf einem
  Worker-Thread weiterlaufen (deshalb `build_tokio_runtime()`, nicht
  `new_current_thread()`).

### Protokoll

`docs/protocol.md` ist die verbindliche Schnittstelle. Frames werden tolerant
geparst (`protocol::codec`): unbekannte `type`s und `pusher:*`-Frames (altes Transport-Protokoll)
werden ignoriert, nie als Verbindungsfehler behandelt. `round_state.countdown_ms`
ist nur während `countdown` gültig – der Server sendet außerhalb `0`.
