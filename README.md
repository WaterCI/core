# WaterCore

Contient l'ordonanceur, celui qui va recevoir et distribuer les jobs.

```plantuml
@startuml
package inputs {
[WebUI]
[WebHook]
[RepoWatcher]
}
package outputs {
[WebUI]
[Core] -> [WebUi]: WebSocket
}
[WebUI] -> input_queue: InputPayload
[WebHook] -> input_queue: : InputPayload
[RepoWatcher] -> input_queue: : InputPayload
input_queue -> [Core]
@enduml
```

J'ai besoin de:
- Une interface TCP pour recevoir des jobs
- Une queue interne pour stocker ces jobs
- Une interface TCP pour recevoir les connexions des exécuteurs
- Boucler sur ces exécuteurs pour dispatcher les exécuteurs