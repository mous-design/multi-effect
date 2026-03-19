# Nog te bouwen

## Web-frontend
Er moet een webinterface komen waar ieder effect zichtbaar is als een tile met x-aantal controls. De tiles zijn versleepbaar (volgorde in de chain). Daarnaast een config-menuutje/popup met systeeminstellingen.

Ik denk aan react als basis hiervoor.

De controls moeten livereageren als een andere bron iets verandert. Dat is belangrijk.

Nodig in dit project:
• JSON-api
• rudimentaire hosting voor de html-pagina
• WebSocket pub/sub voor live updates naar de UI

## Physical controller
Er moet een fysieke controller komen die ik zelf ga maken.

De controls moeten livereageren als een andere bron iets verandert. Dat is belangrijk.

Ik denk aan:
• een microcontroller.
• een (USB) serial interface.
• knoppen (gray-encoders en foot-switches).
• Display(s) voor de huidige settings - makkelijks is 1 lcd tekst display.

## Presets
• controller-mapping(s) — nog te doen

### Preset change
• controller moet het kunnen (up/down, getalsmatig) — nog te doen
• webinterface ook — nog te doen

## Gedachtes
Meerdere instanties van alles: meerdere web-interfaces, meerdere controllers, meerdere MIDI-sources. Consequent doorvoeren.

## MIDI mappings
Globale kanaalwissel toepassen op meerdere presets tegelijk

## Controller mappings
Vergelijkbaar met midi-mapper. Virtual inputs: map een named input naar een serial port. Nog nader uit te denken.
