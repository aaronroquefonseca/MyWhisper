//! Interface strings: English + Spanish, keyed by tag.
//!
//! Every user-visible string in the desktop app lives here with both
//! languages. Call `tr("tag")` (static) or `trf("tag", &[("name",
//! value)])` for `{placeholders}`. Language is process-global:
//! `System` follows `$LANG` (Spanish when it starts with `es`,
//! English otherwise), `En`/`Es` pin it.

use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UiLang {
    System,
    En,
    Es,
}

impl UiLang {
    pub fn parse(s: &str) -> Self {
        match s {
            "en" => UiLang::En,
            "es" => UiLang::Es,
            _ => UiLang::System,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            UiLang::System => "system",
            UiLang::En => "en",
            UiLang::Es => "es",
        }
    }
}

static LANG: AtomicU8 = AtomicU8::new(0);

pub fn set_lang(lang: UiLang) {
    LANG.store(
        match lang {
            UiLang::System => 0,
            UiLang::En => 1,
            UiLang::Es => 2,
        },
        Ordering::SeqCst,
    );
}

pub fn lang() -> UiLang {
    match LANG.load(Ordering::SeqCst) {
        1 => UiLang::En,
        2 => UiLang::Es,
        _ => UiLang::System,
    }
}

fn system_is_spanish() -> bool {
    std::env::var("LANGUAGE")
        .or_else(|_| std::env::var("LANG"))
        .map(|v| {
            v.split(':')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase()
                .starts_with("es")
        })
        .unwrap_or(false)
}

fn spanish() -> bool {
    match lang() {
        UiLang::Es => true,
        UiLang::En => false,
        UiLang::System => system_is_spanish(),
    }
}

/// Look up a string tag. Unknown tags render as-is (leaked once):
/// a missing entry must be visible, never blank.
pub fn tr(tag: &str) -> &'static str {
    let Some((en, es)) = try_lookup(tag) else {
        eprintln!("[i18n] missing string tag: {tag}");
        return Box::leak(tag.to_string().into_boxed_str());
    };
    if spanish() {
        es
    } else {
        en
    }
}

/// Look up a string with `{placeholder}` substitution.
pub fn trf(tag: &str, args: &[(&str, &str)]) -> String {
    let mut out = tr(tag).to_string();
    for (k, v) in args {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

// ---------------------------------------------------------------- rebuild

std::thread_local! {
    static REBUILD: RefCell<Option<Rc<dyn Fn()>>> = RefCell::new(None);
}

/// The app shell registers a window rebuild here; the language picker
/// calls `rebuild_ui()` after persisting, so the new language applies
/// immediately (modal state is not preserved across the rebuild).
pub fn set_rebuild_hook(hook: Rc<dyn Fn()>) {
    REBUILD.with(|r| *r.borrow_mut() = Some(hook));
}

pub fn rebuild_ui() {
    // Take (not clone-under-borrow): the hook rebuilds the window,
    // which registers a fresh hook — holding the borrow across the
    // call would panic on re-entrant borrow_mut.
    let hook = REBUILD.with(|r| r.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

// ---------------------------------------------------------------- table

fn try_lookup(tag: &str) -> Option<(&'static str, &'static str)> {
    Some(match tag {
        // Sidebar + app chrome
        "app.name" => ("MyWhisper", "MyWhisper"),
        "nav.home" => ("Home", "Inicio"),
        "nav.insights" => ("Insights", "Estadísticas"),
        "nav.dictionary" => ("Dictionary", "Diccionario"),
        "nav.sync" => ("Sync", "Sincronizar"),
        "nav.settings" => ("Settings", "Ajustes"),

        // Settings sections
        "settings.preferences" => ("Preferences", "Preferencias"),
        "settings.preferences.sub" => (
            "General application behavior.",
            "Comportamiento general de la aplicación.",
        ),
        "settings.hotkeys" => ("Hotkeys", "Atajos"),
        "settings.clipboard" => ("Clipboard", "Portapapeles"),
        "settings.clipboard.sub" => (
            "Automatic pasting and clipboard behavior.",
            "Pegado automático y comportamiento del portapapeles.",
        ),
        "settings.hotkeys.sub" => (
            "Global shortcuts, read straight from your keyboards.\nHold = talk while held. Toggle = press to start/stop. Smart = quick tap toggles, longer press holds.",
            "Atajos globales, leídos directamente de tus teclados.\nMantener = hablar mientras se pulsa. Alternar = pulsar para empezar/terminar. Smart = toque rápido alterna, pulsación larga mantiene.",
        ),
        "settings.stt" => ("Speech to Text", "Voz a texto"),
        "settings.stt.sub" => (
            "How your voice is transcribed.",
            "Cómo se transcribe tu voz.",
        ),
        "settings.lm" => ("Language Models", "Modelos de lenguaje"),
        "settings.lm.sub" => (
            "Cleanup and assistant models. Ollama-first.",
            "Modelos de limpieza y asistente. Ollama primero.",
        ),
        "settings.privacy" => ("Privacy & Data", "Privacidad y datos"),
        "settings.privacy.sub" => ("Retention and history.", "Retención e historial."),
        "settings.system" => ("System", "Sistema"),
        "settings.system.sub" => ("Version and local data.", "Versión y datos locales."),
        // Groups
        "group.appearance" => ("Appearance", "Apariencia"),
        "group.sound" => ("Sound", "Sonido"),
        "group.clipboard" => ("Clipboard", "Portapapeles"),
        "group.paste" => ("Paste injection", "Inyección de pegado"),
        "group.floating" => ("Floating pill", "Píldora flotante"),
        "group.language" => ("Language", "Idioma"),
        "group.microphone" => ("Microphone", "Micrófono"),
        "group.dictionary" => ("Dictionary", "Diccionario"),
        "group.dictation" => ("Dictation", "Dictado"),
        "group.smart" => ("Smart timing", "Tiempo smart"),
        "group.engine" => ("Engine", "Motor"),
        "group.cloud" => ("Cloud provider (BYOK)", "Proveedor en la nube (BYOK)"),
        "group.selfhosted" => ("Self-hosted", "Autohospedado"),
        "group.vad" => ("Voice activity", "Actividad de voz"),
        "group.provider" => ("Provider", "Proveedor"),
        "group.cloudfallback" => ("Cloud fallback (BYOK)", "Respaldo en la nube (BYOK)"),
        "group.cleanup" => ("Dictation cleanup", "Limpieza de dictados"),
        "group.audioret" => ("Audio retention", "Retención de audio"),
        "group.transcripts" => ("Transcripts", "Transcripciones"),
        "group.version" => ("Version", "Versión"),
        "group.modelcache" => ("Model cache", "Caché de modelos"),
        "group.danger" => ("Danger zone", "Zona de peligro"),
        // Rows
        "row.theme" => ("Theme", "Tema"),
        "row.dictsounds" => ("Dictation sounds", "Sonidos de dictado"),
        "row.pausemedia" => ("Pause media while dictating", "Pausar multimedia al dictar"),
        "row.autopaste" => (
            "Auto-paste transcription",
            "Pegar transcripción automáticamente",
        ),
        "row.keepclip" => (
            "Keep transcription in clipboard",
            "Mantener la transcripción en el portapapeles",
        ),
        "row.copybackend" => ("Copy backend", "Motor de copiado"),
        "row.hidehistory" => (
            "Hide transcription from clipboard history",
            "Ocultar la transcripción del historial del portapapeles",
        ),
        "row.pastebackend" => ("Paste backend", "Motor de pegado"),
        "row.pastecombo" => ("Paste shortcut", "Atajo de pegado"),
        "status.available" => ("Available", "Disponible"),
        "status.missing" => ("Not found", "No encontrado"),
        "row.startpos" => ("Start position", "Posición inicial"),
        "row.uilang" => ("Interface language", "Idioma de la interfaz"),
        "row.sttlang" => ("Transcription language", "Idioma de transcripción"),
        "row.micinput" => ("Input", "Entrada"),
        "row.deviceid" => ("Device id", "ID de dispositivo"),
        "row.micwarm" => ("Keep microphone warm", "Mantener el micrófono activo"),
        "row.micunlisted" => ("Disconnected: ", "Desconectado: "),
        "row.autolearn" => (
            "Learn words from corrections",
            "Aprender palabras de correcciones",
        ),
        "row.activation" => ("Activation", "Activación"),
        "row.engine" => ("Engine", "Motor"),
        "opt.whisperlocal" => ("Whisper (local)", "Whisper (local)"),
        "opt.parakeetlocal" => ("Parakeet (local)", "Parakeet (local)"),
        "opt.selfhostedserver" => ("Self-hosted server", "Servidor propio"),
        "stt.models.title" => ("Models", "Modelos"),
        "action.use" => ("Use", "Usar"),
        "action.using" => ("Using", "En uso"),
        "action.download" => ("Download", "Descargar"),
        "action.installing" => ("Installing…", "Instalando…"),
        "action.retry" => ("Retry", "Reintentar"),
        "stt.dl.failed" => ("Failed: {err}", "Falló: {err}"),
        "stt.sherpa.installed" => ("Installed", "Instalado"),
        "stt.gpu.title" => ("Runtimes", "Runtimes"),
        "stt.gpu.sherpa" => (
            "These models run on the integrated sherpa runtime — server packs above do not apply.",
            "Estos modelos usan el runtime sherpa integrado — los paquetes de servidor no aplican.",
        ),
        "stt.gpu.backend" => ("Backend", "Backend"),
        "stt.gpu.cpu" => ("CPU", "CPU"),
        "stt.gpu.cuda" => ("CUDA (NVIDIA)", "CUDA (NVIDIA)"),
        "stt.gpu.vulkan" => ("Vulkan (AMD/Intel)", "Vulkan (AMD/Intel)"),
        "stt.gpu.recommend" => ("Detected hardware", "Hardware detectado"),
        "stt.models.dictnote" => (
            "Dictionary hinting only works with OpenAI (whisper) models.",
            "Las sugerencias del diccionario solo funcionan con modelos OpenAI (whisper).",
        ),
        "stt.needle.engine" => ("Needle engine", "Motor Needle"),
        "row.resettodefault" => ("Reset to default", "Restablecer valores"),
        "row.mode" => ("Mode", "Modo"),
        "row.localprovider" => ("Local provider", "Proveedor local"),
        "row.whispermodel" => ("Whisper model", "Modelo Whisper"),
        "row.parakeetmodel" => ("Parakeet model", "Modelo Parakeet"),
        "row.provider" => ("Provider", "Proveedor"),
        "row.model" => ("Model", "Modelo"),
        "row.apikey" => ("API key", "Clave API"),
        "row.serverurl" => ("Server URL", "URL del servidor"),
        "row.language" => ("Language", "Idioma"),
        "row.vad" => ("Voice activity detection", "Detección de actividad de voz"),
        "row.vadthreshold" => ("VAD threshold", "Umbral VAD"),
        "row.livepreview" => ("Live transcription preview", "Vista previa en vivo"),
        "row.ollamaurl" => ("Ollama URL", "URL de Ollama"),
        "row.ollamamodel" => ("Ollama model", "Modelo Ollama"),
        "row.localmodel" => ("Local model", "Modelo local"),
        "row.name" => ("Name", "Nombre"),
        "lm.use" => ("Use LLM post-processing", "Usar postprocesado LLM"),
        "row.lmtype" => ("Type", "Tipo"),
        "group.lmadd" => ("Add option", "Añadir opción"),
        "group.lmchain" => ("Configured options", "Opciones configuradas"),
        "group.lmpost" => ("Post-processing", "Postprocesado"),
        "lm.local.note" => (
            "Local self-managed engines are not implemented yet — use Ollama.",
            "Los motores locales autogestionados aún no están implementados: usa Ollama.",
        ),
        "lm.list.empty" => (
            "No options yet: configure one above and press Add.",
            "Aún no hay opciones: configura una arriba y pulsa Añadir.",
        ),
        "row.keeploaded" => ("Keep model loaded", "Mantener el modelo cargado"),
        "row.customprompt" => ("Custom prompt", "Prompt personalizado"),
        "row.cleanup" => ("Clean up transcriptions", "Limpiar transcripciones"),
        "row.keepaudio" => ("Keep audio recordings", "Conservar grabaciones de audio"),
        "row.storedaudio" => ("Stored audio", "Audio guardado"),
        "row.storedtranscripts" => ("Stored transcripts", "Transcripciones guardadas"),
        "row.keephistory" => ("Keep dictation history", "Guardar historial de dictados"),
        "row.autodelete" => ("Auto-delete transcripts", "Borrado automático"),
        "row.autodeleteaudio" => ("Auto-delete audio", "Borrado de audio"),
        "row.savediscarded" => (
            "Save discarded transcriptions",
            "Guardar transcripciones descartadas",
        ),
        "row.appversion" => ("Application version", "Versión de la aplicación"),
        "row.cachepath" => ("Cache path", "Ruta de caché"),
        "row.cacheactions" => ("Cache actions", "Acciones de caché"),
        "row.resetall" => ("Reset all application data", "Restablecer todos los datos"),
        "row.animation" => ("Animation", "Animación"),
        "row.bars" => ("Bars", "Barras"),
        "row.height" => ("Height", "Altura"),
        "row.viztheme" => ("Theme", "Tema"),
        "row.ornament" => ("Ornament", "Ornamento"),
        "row.idle" => ("Idle animation", "Animación en reposo"),
        "row.addcombo" => (
            "Add combo (e.g. F13, Ctrl+Alt+D)",
            "Añadir combinación (p. ej. F13, Ctrl+Alt+D)",
        ),
        // Dropdown options
        "opt.system" => ("System", "Sistema"),
        "opt.light" => ("Light", "Claro"),
        "opt.dark" => ("Dark", "Oscuro"),
        "opt.hold" => ("Hold", "Mantener"),
        "opt.toggle" => ("Toggle", "Alternar"),
        "opt.smart" => ("Smart", "Smart"),
        "opt.bottomcenter" => ("Bottom center", "Abajo centro"),
        "opt.bottomleft" => ("Bottom left", "Abajo izquierda"),
        "opt.bottomright" => ("Bottom right", "Abajo derecha"),
        "opt.sysdefault" => ("System default", "Por defecto del sistema"),
        "opt.builtin" => ("Built-in", "Integrado"),
        "opt.specific" => ("Specific device", "Dispositivo específico"),
        "opt.off" => ("Off", "Desactivado"),
        "opt.hold10s" => ("10 seconds", "10 segundos"),
        "opt.hold60s" => ("60 seconds", "60 segundos"),
        "opt.hold15m" => ("15 minutes", "15 minutos"),
        "opt.local" => ("Local (on-device)", "Local (en el dispositivo)"),
        "opt.cloud" => ("Cloud provider", "Proveedor en la nube"),
        "opt.selfhosted" => ("Self-hosted", "Autohospedado"),
        "opt.ollama" => ("Ollama", "Ollama"),
        "opt.localmodel" => ("Local model", "Modelo local"),
        "opt.disabled" => ("Disabled", "Desactivado"),
        "opt.forever" => ("Forever", "Para siempre"),
        "opt.never" => ("Never", "Nunca"),
        "opt.day1" => ("1 day", "1 día"),
        "opt.older1" => ("Older than 1 day", "Anteriores a 1 día"),
        "opt.older7" => ("Older than 7 days", "Anteriores a 7 días"),
        "opt.older14" => ("Older than 14 days", "Anteriores a 14 días"),
        "opt.older30" => ("Older than 30 days", "Anteriores a 30 días"),
        "opt.older60" => ("Older than 60 days", "Anteriores a 60 días"),
        "opt.older90" => ("Older than 90 days", "Anteriores a 90 días"),
        "opt.day7" => ("7 days", "7 días"),
        "opt.day14" => ("14 days", "14 días"),
        "opt.day30" => ("30 days", "30 días"),
        "opt.day60" => ("60 days", "60 días"),
        "opt.day90" => ("90 days", "90 días"),
        "opt.tape" => ("Tape", "Cinta"),
        "opt.history" => ("History", "Historial"),
        "opt.white" => ("White", "Blanco"),
        "opt.grey" => ("Grey", "Gris"),
        "opt.red" => ("Red", "Rojo"),
        "opt.none" => ("None", "Ninguno"),
        "opt.mark" => ("Mark", "Marca"),
        "opt.mic" => ("Mic", "Micro"),
        "opt.dot" => ("Dot", "Punto"),
        "opt.still" => ("Still", "Quieto"),
        "opt.silhouette" => ("Silhouette", "Silueta"),
        "opt.calm" => ("Calm", "Calma"),
        // Hotkeys capture UI
        "hotkeys.record" => ("Record", "Grabar"),
        "hotkeys.recording" => ("Press keys… (Esc cancels)", "Pulsa teclas… (Esc cancela)"),
        "hotkeys.presskeys" => ("Press keys", "Pulsa teclas"),
        "hotkeys.invalidcombo" => ("Invalid combo, try F13", "Combinación inválida, prueba F13"),
        "hotkeys.invalid" => ("Invalid, try again", "Inválido, inténtalo de nuevo"),
        "hotkeys.unsupported" => (
            "Unsupported key, try again",
            "Tecla no soportada, inténtalo de nuevo",
        ),
        "hotkeys.pressnow" => ("Press keys now…", "Pulsa teclas ahora…"),
        "hotkeys.recordhint" => (
            "Click, then press the key combo",
            "Clic y pulsa la combinación",
        ),
        "hotkeys.behavior" => (
            "Behavior for the new binding",
            "Comportamiento del nuevo atajo",
        ),
        // Smart timing
        "smart.label" => (
            "Tap ceiling (ms): shorter taps toggle, longer holds",
            "Umbral de toque (ms): toques cortos alternan, largos mantienen",
        ),
        // Home
        "home.greeting" => ("Welcome back", "Te damos la bienvenida"),
        "home.subtitle" => (
            "Press your dictation hotkey anywhere to start writing with your voice.",
            "Pulsa tu atajo de dictado en cualquier lugar para empezar a escribir con tu voz.",
        ),
        "home.empty.title" => ("No dictations yet", "Aún no hay dictados"),
        "home.empty.hint" => (
            "Your history will appear here.\nDefault hotkey: {hotkey}",
            "Tu historial aparecerá aquí.\nAtajo por defecto: {hotkey}",
        ),
        "home.entry.meta" => ("{date} · {words} words", "{date} · {words} palabras"),
        "home.today" => ("Today", "Hoy"),
        "home.yesterday" => ("Yesterday", "Ayer"),
        "home.copy" => ("Copy", "Copiar"),
        "home.delete" => ("Delete", "Eliminar"),
        "home.viewraw" => ("View raw transcript", "Ver transcripción original"),
        "home.raw.title" => ("RAW TRANSCRIPT", "TRANSCRIPCIÓN ORIGINAL"),
        "home.raw.passthrough" => (
            "No AI processing was applied to this transcription",
            "No se aplicó procesamiento IA a esta transcripción",
        ),
        "home.wpm" => ("{wpm} wpm", "{wpm} ppm"),
        "home.showdiscarded" => ("Show discarded", "Mostrar descartados"),
        "home.hidediscarded" => ("Hide discarded", "Ocultar descartados"),
        "home.discarded" => ("discarded", "descartado"),
        // Insights
        "insights.title" => ("Insights", "Estadísticas"),
        "insights.subtitle" => (
            "Your usage, computed on-device.",
            "Tu uso, calculado en este dispositivo.",
        ),
        "insights.words" => ("Words spoken", "Palabras dictadas"),
        "insights.alltime" => ("All time", "Desde siempre"),
        "insights.wpm" => ("Words per minute", "Palabras por minuto"),
        "insights.wpm.caption" => (
            "Based on 100% of measured words",
            "Basado en el 100% de palabras medidas",
        ),
        "insights.dictations" => ("Dictations", "Dictados"),
        "insights.streak" => ("Current streak", "Racha actual"),
        "insights.streak.value" => ("{n} day{s}", "{n} día{s}"),
        "insights.streak.caption" => ("Longest: {n} day{s}", "Récord: {n} día{s}"),
        "insights.activity" => ("Speaking activity", "Actividad de habla"),
        "insights.less" => ("Less", "Menos"),
        "insights.more" => ("More", "Más"),
        "insights.heat.tooltip" => ("{date}: {words} {unit}", "{date}: {words} {unit}"),
        "insights.word.one" => ("word", "palabra"),
        "insights.word.many" => ("words", "palabras"),
        // Shared actions
        "action.add" => ("Add", "Añadir"),
        "action.delete" => ("Delete", "Eliminar"),
        "action.edit" => ("Edit", "Editar"),
        "action.save" => ("Save", "Guardar"),
        "action.cancel" => ("Cancel", "Cancelar"),
        // Settings actions
        "action.clearallaudio" => ("Clear all audio", "Borrar todo el audio"),
        "action.clearalltranscripts" => ("Clear all transcripts", "Borrar transcripciones"),
        "action.clearcache" => ("Clear cache", "Limpiar caché"),
        "action.openfolder" => ("Open folder", "Abrir carpeta"),
        "action.resetdots" => ("Reset...", "Restablecer..."),
        "action.sendrequest" => ("Send request", "Enviar solicitud"),
        "action.syncnow" => ("Sync now", "Sincronizar ahora"),
        "action.check" => ("Check", "Comprobar"),
        "action.start" => ("Start", "Iniciar"),
        "action.stop" => ("Stop", "Detener"),
        "action.accept" => ("Accept", "Aceptar"),
        "action.decline" => ("Decline", "Rechazar"),
        "hotkeys.holdlong" => ("Hold to talk", "Mantener para hablar"),
        "hotkeys.presstoggle" => ("Press to toggle", "Pulsar para alternar"),
        // Sync
        "sync.never" => ("never", "nunca"),
        "sync.ago.now" => ("just now", "ahora mismo"),
        "sync.ago.min" => ("{n} min ago", "hace {n} min"),
        "sync.ago.h" => ("{n} h ago", "hace {n} h"),
        "sync.ago.d" => ("{n} d ago", "hace {n} d"),
        "sync.noaddr" => ("no address", "sin dirección"),
        "sync.lastsync" => ("last sync {ago}", "última sincronización {ago}"),
        "sync.trusted" => ("Trusted device", "Dispositivo de confianza"),
        "sync.syncing" => ("Syncing with {id}…", "Sincronizando con {id}…"),
        "sync.synced" => (
            "Synced with {id}: {words} words, {snippets} snippets, {history} history in",
            "Sincronizado con {id}: {words} palabras, {snippets} fragmentos, {history} historial",
        ),
        "sync.failed" => ("Sync failed: {err}", "Falló la sincronización: {err}"),
        "sync.err.untrusted" => (
            "Device is not trusted.",
            "El dispositivo no es de confianza.",
        ),
        "sync.err.noaddr" => (
            "No known address for this device yet.",
            "Aún no se conoce dirección para este dispositivo.",
        ),
        "sync.err.accept" => (
            "Could not accept request.",
            "No se pudo aceptar la solicitud.",
        ),
        "sync.err.save" => ("Could not save peer.", "No se pudo guardar el par."),
        "sync.err.badid" => (
            "Need the peer's 64-char node id.",
            "Se necesita el id de nodo de 64 caracteres.",
        ),
        "sync.accepted" => (
            "Device accepted. Syncing…",
            "Dispositivo aceptado. Sincronizando…",
        ),
        "sync.declined" => ("Request declined.", "Solicitud rechazada."),
        "sync.listener.off" => (
            "Sync listener is off.",
            "El receptor de sincronización está apagado.",
        ),
        "sync.listener.on" => (
            "Listening for sync on port {port}.",
            "Escuchando sincronización en el puerto {port}.",
        ),
        "sync.reach.unknown" => ("Reachability unknown.", "Alcance desconocido."),
        "sync.reach.checking" => ("Checking…", "Comprobando…"),
        "sync.reach.down" => (
            "Sync listener is not answering on port {port}.",
            "El receptor no responde en el puerto {port}.",
        ),
        "sync.reach.open" => (
            "Listener up, firewall open for {port}/tcp.",
            "Receptor activo, firewall abierto para {port}/tcp.",
        ),
        "sync.reach.blocked" => (
            "Listener is up, but the firewall blocks port {port}/tcp, so peers cannot reach you. Run: sudo firewall-cmd --permanent --add-port={port}/tcp && sudo firewall-cmd --reload",
            "El receptor está activo, pero el firewall bloquea el puerto {port}/tcp. Ejecuta: sudo firewall-cmd --permanent --add-port={port}/tcp && sudo firewall-cmd --reload",
        ),
        "sync.reach.unknownfw" => (
            "Listener is up on port {port} (firewall state unknown).",
            "Receptor activo en el puerto {port} (estado del firewall desconocido).",
        ),
        "sync.svc.title" => ("Background service", "Servicio en segundo plano"),
        "sync.svc.on" => (
            "Running: hotkeys, sync and pill are live.",
            "En marcha: atajos, sincronización y píldora activos.",
        ),
        "sync.svc.off" => (
            "Stopped: hotkeys, sync and the pill are idle. Start it to restore full functionality.",
            "Detenido: atajos, sincronización y píldora inactivos. Inícialo para recuperar la funcionalidad.",
        ),
        "sync.device.title" => ("This device", "Este dispositivo"),
        "sync.device.name" => ("Device name", "Nombre del dispositivo"),
        "sync.device.id" => ("Node id", "ID de nodo"),
        "sync.device.port" => ("Listen port", "Puerto de escucha"),
        "sync.device.porthint" => (
            "0 = off. Restart applies.",
            "0 = apagado. Requiere reinicio.",
        ),
        "sync.add.title" => ("Add device", "Añadir dispositivo"),
        "sync.add.hint" => (
            "Paste the peer's node id. Addresses are discovered automatically on the local network; the field below is only needed for devices that cannot be discovered (VPN hosts, manual IPs). Verify the id with them out-of-band, then they accept your request on their side.",
            "Pega el id de nodo del par. Las direcciones se descubren automáticamente en la red local; el campo siguiente solo es necesario para dispositivos no detectables (VPN, IPs manuales). Verifica el id con ellos por otro medio, y aceptarán tu solicitud en su lado.",
        ),
        "sync.add.idhint" => ("Peer node id (64 hex chars)", "ID de nodo del par (64 hex)"),
        "sync.add.addrhint" => (
            "Address override, e.g. 192.168.1.31:42973 (optional)",
            "Dirección manual, p. ej. 192.168.1.31:42973 (opcional)",
        ),
        "sync.add.namehint" => ("Name (optional)", "Nombre (opcional)"),
        "sync.nearby.title" => ("Nearby", "Cercanos"),
        "sync.nearby.empty" => (
            "No new devices seen on this network yet.",
            "Aún no se ven dispositivos nuevos en esta red.",
        ),
        "sync.requests.title" => ("Requests", "Solicitudes"),
        "sync.requests.empty" => ("No pending requests.", "No hay solicitudes pendientes."),
        "sync.requests.row" => (
            "{name} · {id} · seen {seen}",
            "{name} · {id} · visto {seen}",
        ),
        "sync.devices.title" => ("Devices", "Dispositivos"),
        "sync.devices.empty" => (
            "No devices yet. Add one by node id, or wait for a request.",
            "Aún no hay dispositivos. Añade uno por id de nodo o espera una solicitud.",
        ),
        "sync.request.sent" => (
            "Request recorded for {id}. They accept on their side, then sync starts.",
            "Solicitud registrada para {id}. La aceptarán en su lado y empezará la sincronización.",
        ),
        "sync.request.noaddr" => (
            "Request recorded for {id}. No address yet: they appear under Nearby on the same network, or add an address.",
            "Solicitud registrada para {id}. Sin dirección aún: aparecerán en Cercanos en la misma red, o añade una dirección.",
        ),
        "sync.request.reaching" => ("Reaching {id}…", "Contactando {id}…"),
        "sync.request.delivered" => ("Delivered and synced.", "Entregado y sincronizado."),
        "sync.request.unreachable" => (
            "Unreachable right now ({err}). Kept; retry from Devices.",
            "Inalcanzable ahora ({err}). Se conserva; reintenta desde Dispositivos.",
        ),
        "action.close" => ("Close", "Cerrar"),
        "action.copy" => ("Copy", "Copiar"),
        "action.remove" => ("Remove", "Quitar"),
        "action.reset" => ("Reset", "Restablecer"),
        "action.create" => ("Create", "Crear"),
        // Dictionary
        "dict.title" => ("Dictionary", "Diccionario"),
        "dict.tab.words" => ("Dictionary", "Diccionario"),
        "dict.tab.snippets" => ("Snippets", "Fragmentos"),
        "dict.words.empty" => (
            "No words yet. Added words are always recognized.",
            "Aún no hay palabras. Las palabras añadidas siempre se reconocen.",
        ),
        "dict.words.add" => ("Add a word...", "Añade una palabra..."),
        "dict.snippets.empty" => (
            "No snippets yet. Type a trigger anywhere to expand it.",
            "Aún no hay fragmentos. Escribe un disparador en cualquier lugar para expandirlo.",
        ),
        "dict.snippets.trigger" => ("Trigger, e.g. omw", "Disparador, p. ej. omw"),
        "dict.snippets.create" => ("Create snippet", "Crear fragmento"),
        _ => return None,
    })
}
