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

pub(crate) fn spanish() -> bool {
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
    if spanish() { es } else { en }
}

/// Look up a string with `{placeholder}` substitution.
pub fn trf(tag: &str, args: &[(&str, &str)]) -> String {
    let mut out = tr(tag).to_string();
    for (k, v) in args {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

/// Translate registry presentation text while preserving engine IDs and backend data.
pub(crate) fn backend_text(text: &'static str) -> &'static str {
    let tag = match text {
        "Fastest, lower quality" => "backend.caption.0",
        "Good balance" => "backend.caption.1",
        "Better quality, slower" => "backend.caption.2",
        "High quality" => "backend.caption.3",
        "Best quality, slowest" => "backend.caption.4",
        "Fast with good quality" => "backend.caption.5",
        "16.9 MB STT, 7 languages, own CPU engine" => "backend.caption.6",
        "Fast multilingual ASR with auto language detection" => "backend.caption.7",
        "English-only, fast" => "backend.caption.8",
        "English streaming" => "backend.caption.9",
        "Multilingual streaming, 15 languages" => "backend.caption.10",
        "14 languages, no auto-detect (single-language server)" => "backend.caption.11",
        "Multilingual Parakeet fine-tune (CC-BY-SA-4.0)" => "backend.caption.12",
        "Portable whisper-server, works everywhere" => "backend.caption.13",
        "NVIDIA GPUs (Pascal 6.1+, needs proprietary driver)" => "backend.caption.14",
        "AMD/Intel/discrete GPUs via Vulkan" => "backend.caption.15",
        "Offline + online websocket servers, CUDA 13 (needs system CUDA libs)" => {
            "backend.caption.16"
        }
        "Offline + online websocket servers, onnxruntime included" => "backend.caption.17",
        "CPU server" => "backend.caption.18",
        "CUDA server" => "backend.caption.19",
        "Vulkan server" => "backend.caption.20",
        "Sherpa CUDA server" => "backend.caption.21",
        "Sherpa CPU server" => "backend.caption.22",
        _ => return text,
    };
    tr(tag)
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
        "nav.home" => ("Dictation", "Dictado"),
        "nav.insights" => ("Activity", "Actividad"),
        "nav.dictionary" => ("Dictionary", "Diccionario"),
        "nav.sync" => ("Devices", "Dispositivos"),
        "nav.settings" => ("Settings", "Ajustes"),

        // Settings sections
        "settings.preferences" => ("General", "General"),
        "settings.preferences.sub" => (
            "Choose your appearance, microphone, and recording indicator.",
            "Elige la apariencia, el micrófono y el indicador de grabación.",
        ),
        "settings.hotkeys" => ("Shortcuts", "Atajos"),
        "settings.clipboard" => ("Text delivery", "Entrega de texto"),
        "settings.clipboard.sub" => (
            "Choose how finished dictations reach your apps and clipboard.",
            "Elige cómo llegan los dictados terminados a tus aplicaciones y al portapapeles.",
        ),
        "settings.hotkeys.sub" => (
            "Hold to talk: release to finish. Press to toggle: press again to finish. Smart: tap to toggle, or hold to talk.",
            "Mantener para hablar: suelta para terminar. Pulsar para alternar: pulsa de nuevo para terminar. Inteligente: toca para alternar o mantén para hablar.",
        ),
        "settings.stt" => ("Transcription", "Transcripción"),
        "settings.stt.sub" => (
            "Turn speech into text. Enabled engines are tried from top to bottom until one succeeds. Cloud engines send audio to the selected provider.",
            "Convierte voz en texto. Los motores activos se prueban en orden hasta que uno funciona. Los motores en la nube envían el audio al proveedor elegido.",
        ),
        "settings.lm" => ("Language models", "Modelos de lenguaje"),
        "settings.lm.sub" => (
            "Manage language models and their connections. Choose how they are used in each feature’s settings.",
            "Gestiona los modelos de lenguaje y sus conexiones. Elige cómo se usan en los ajustes de cada función.",
        ),
        "settings.cleanup" => ("Text cleanup", "Edición de texto"),
        "settings.cleanup.sub" => (
            "Polish dictations after transcription.",
            "Mejora los dictados después de transcribirlos.",
        ),
        "settings.privacy" => ("Privacy & storage", "Privacidad y almacenamiento"),
        "settings.privacy.sub" => (
            "Control what is saved on this device and how long it is kept.",
            "Controla qué se guarda en este dispositivo y durante cuánto tiempo.",
        ),
        "settings.system" => ("System", "Sistema"),
        "settings.system.sub" => (
            "Check for updates and manage downloaded models and app data.",
            "Busca actualizaciones y gestiona los modelos descargados y los datos de la aplicación.",
        ),
        // Groups
        "group.appearance" => ("Appearance", "Apariencia"),
        "group.sound" => ("Sound", "Sonido"),
        "group.clipboard" => ("Clipboard", "Portapapeles"),
        "group.paste" => ("Automatic paste", "Pegado automático"),
        "group.floating" => ("Recording indicator", "Indicador de grabación"),
        "group.language" => ("Language", "Idioma"),
        "group.microphone" => ("Microphone", "Micrófono"),
        "group.dictation" => ("Dictation", "Dictado"),
        "group.smart" => ("Smart shortcut timing", "Tiempo del atajo inteligente"),
        "group.engine" => ("Engine", "Motor"),
        "group.cloud" => (
            "Cloud provider · your API key",
            "Proveedor en la nube · tu clave API",
        ),
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
        "row.copybackend" => ("Clipboard tool", "Herramienta de portapapeles"),
        "row.hidehistory" => (
            "Hide transcription from clipboard history",
            "Ocultar la transcripción del historial del portapapeles",
        ),
        "row.pastebackend" => ("Paste tool", "Herramienta de pegado"),
        "row.pastecombo" => ("Paste shortcut", "Atajo de pegado"),
        "status.available" => ("Available", "Disponible"),
        "status.missing" => ("Not found", "No encontrado"),
        "row.startpos" => ("Start position", "Posición inicial"),
        "row.uilang" => ("Interface language", "Idioma de la interfaz"),
        "row.sttlang" => ("Transcription language", "Idioma de transcripción"),
        "row.micinput" => ("Input", "Entrada"),
        "row.deviceid" => ("Device id", "ID de dispositivo"),
        "row.micwarm" => ("Keep microphone ready", "Mantener el micrófono preparado"),
        "row.micunlisted" => ("Disconnected: ", "Desconectado: "),
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
        "stt.sherpa.backend" => ("Sherpa backend", "Motor Sherpa"),
        "stt.sherpa.cpu" => ("CPU", "CPU"),
        "stt.sherpa.cuda" => ("CUDA", "CUDA"),
        "group.sttchain" => ("Transcription priority", "Prioridad de transcripción"),
        "stt.local.add" => ("Add on-device engine", "Añadir motor local"),
        "stt.models.compatnote" => (
            "Not all models support dictionary hinting and live streaming (lower latency).",
            "No todos los modelos admiten sugerencias del diccionario ni streaming en directo (menor latencia).",
        ),
        "stt.models.full" => ("Fully supported", "Totalmente compatible"),
        "stt.models.nostream" => ("No live streaming", "Sin streaming en directo"),
        "stt.models.neither" => (
            "No live streaming nor dictionary hinting",
            "Ni streaming en directo ni sugerencias del diccionario",
        ),
        "stt.models.nodict" => ("No dictionary hinting", "Sin sugerencias del diccionario"),
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
        "group.spec" => (
            "Speculative streaming for whisper",
            "Streaming especulativo para whisper",
        ),
        "spec.note" => (
            "Experimental, not recommended. Decodes while you speak to reduce latency on long holds.",
            "Experimental, no recomendado. Decodifica mientras hablas para reducir la latencia en dictados largos.",
        ),
        "row.specenable" => ("Speculative passes", "Pasadas especulativas"),
        "row.specupdate" => ("Update every (s)", "Actualizar cada (s)"),
        "row.specoverlap" => ("Overlap (s)", "Solapamiento (s)"),
        "row.livepreview" => ("Live transcription preview", "Vista previa en vivo"),
        "stt.live.note" => (
            "Only works with streaming models.",
            "Solo funciona con modelos con streaming.",
        ),
        "row.ollamaurl" => ("Ollama URL", "URL de Ollama"),
        "row.ollamamodel" => ("Ollama model", "Modelo Ollama"),
        "row.localmodel" => ("Local model", "Modelo local"),
        "row.name" => ("Name", "Nombre"),
        "row.keepalive" => ("Keep alive", "Mantener activo"),
        "opt.thinkingdefault" => ("Default", "Predeterminado"),
        "opt.thinkingon" => ("On", "Activado"),
        "opt.thinkingoff" => ("Off", "Desactivado"),
        "row.thinking" => ("Thinking", "Razonamiento"),
        "row.numctx" => ("Context window", "Ventana de contexto"),
        "action.updatenow" => ("Apply to model", "Aplicar al modelo"),
        "action.restore" => ("Restore default", "Restaurar original"),
        "row.lmtype" => ("Type", "Tipo"),
        "group.lmadd" => ("Add a language model", "Añadir un modelo de lenguaje"),
        "group.lmmodels" => ("Configured models", "Modelos configurados"),
        "group.lmchain" => ("Models", "Modelos"),
        "cleanup.priority.note" => (
            "Try enabled models in order. If none succeeds, keep the original text. Cloud models receive your text.",
            "Prueba los modelos activos en orden. Si ninguno funciona, conserva el texto original. Los modelos en la nube reciben tu texto.",
        ),
        "cleanup.models.empty" => (
            "Add a language model to use text cleanup.",
            "Añade un modelo de lenguaje para editar texto.",
        ),
        "lm.usecleanup" => ("Use for text cleanup", "Usar para editar texto"),
        "action.managelm" => ("Manage language models", "Gestionar modelos de lenguaje"),
        "action.cleanupsettings" => ("Text cleanup settings", "Ajustes de edición de texto"),
        "lm.apikey.note" => (
            "Sent as a Bearer token (reverse-proxy setups). Empty means no token.",
            "Se envía como token Bearer (proxies inversos). Vacío significa sin token.",
        ),
        "lm.local.note" => (
            "Local self-managed engines are not implemented yet — use Ollama.",
            "Los motores locales autogestionados aún no están implementados: usa Ollama.",
        ),
        "lm.list.empty" => (
            "No language models configured. Add one to get started.",
            "No hay modelos de lenguaje configurados. Añade uno para empezar.",
        ),
        "row.keeploaded" => ("Keep model loaded", "Mantener el modelo cargado"),
        "row.customprompt" => ("Custom prompt", "Prompt personalizado"),
        "action.editprompt" => ("Edit prompt", "Editar prompt"),
        "prompt.edit.title" => ("Edit cleanup prompt", "Editar prompt de limpieza"),
        "prompt.default" => ("Default prompt", "Prompt predeterminado"),
        "prompt.custom" => ("Custom prompt", "Prompt personalizado"),
        "prompt.default.summary" => (
            "Fix grammar without changing meaning.",
            "Corrige la gramática sin cambiar el sentido.",
        ),
        "action.usedefault" => ("Use default", "Usar predeterminado"),
        "action.fullscreen" => ("Full screen", "Pantalla completa"),
        "action.exitfullscreen" => ("Exit full screen", "Salir de pantalla completa"),
        "row.cleanup" => ("Clean up transcriptions", "Limpiar transcripciones"),
        "row.keepaudio" => ("Keep audio recordings", "Conservar grabaciones de audio"),
        "row.storedaudio" => ("Stored audio", "Audio guardado"),
        "row.storedtranscripts" => ("Stored transcripts", "Transcripciones guardadas"),
        "row.keephistory" => ("Keep dictation history", "Guardar historial de dictados"),
        "row.autodelete" => ("Auto-delete transcripts", "Borrado automático"),
        "group.sttservers" => ("Servers", "Servidores"),
        "row.autostartserver" => ("Auto-start servers", "Iniciar servidores"),
        "row.keepwarm" => ("Keep server ready", "Mantener el servidor preparado"),
        "row.serverstatus" => ("Server status", "Estado del servidor"),
        "server.none" => ("No server running", "Ningún servidor en marcha"),
        "server.running" => ("Running", "En marcha"),
        "server.external" => ("external, not managed", "externo, no gestionado"),
        "servers.note" => (
            "Only one server runs at a time; starting one stops the other.",
            "Solo un servidor funciona a la vez; al iniciar uno se detiene el otro.",
        ),
        "action.coldstart" => ("Start server", "Iniciar servidor"),
        "coldstart.tip.disabled" => (
            "Can't start: add a local option to the chain and enable it.",
            "No se puede iniciar: añade una opción local a la cadena y actívala.",
        ),
        "servers.net.note" => (
            "Not compatible with Whistle (no server: it runs once per dictation).",
            "No compatible con Whistle (sin servidor: se ejecuta una vez por dictado).",
        ),
        "section.network" => ("Network", "Red"),
        "row.netport" => ("Server port", "Puerto del servidor"),
        "net.port.empty" => ("Empty = default", "Vacío = predeterminado"),
        "net.port.tip" => (
            "Empty = default (whisper 8080, sherpa-online 6006, sherpa-offline 6007). Set = that port for whichever server runs. Applies on next server start.",
            "Vacío = predeterminado (whisper 8080, sherpa-online 6006, sherpa-offline 6007). Si se indica, ese puerto para el servidor que se ejecute. Se aplica al próximo arranque.",
        ),
        "row.lanexpose" => ("Expose on local network", "Exponer en la red local"),
        "home.update" => (
            "Update available: {version} — run update.sh to install it.",
            "Actualización disponible: {version} — ejecuta update.sh para instalarla.",
        ),
        "stt.lang.locked.tip" => (
            "Single-language model: fixed.",
            "Modelo de un solo idioma: fijo.",
        ),
        "row.appupdate" => ("Updates", "Actualizaciones"),
        "sys.update.new" => (
            "Update available: {version} — run update.sh to install it.",
            "Actualización disponible: {version} — ejecuta update.sh para instalarla.",
        ),
        "sys.update.current" => (
            "You're on the latest version ({version}).",
            "Tienes la última versión ({version}).",
        ),
        "sys.update.unknown" => ("Not checked yet.", "Aún no comprobado."),
        "sys.update.checking" => ("Checking…", "Comprobando…"),
        "sys.update.failed" => (
            "Couldn't reach releases (offline?).",
            "No se pudo contactar (¿sin conexión?).",
        ),
        "row.lanexpose.note" => (
            "Except for Sherpa: Sherpa always listens on all interfaces, so disabling this option won't take effect for Sherpa.",
            "Excepto Sherpa: Sherpa siempre escucha en todas las interfaces, así que desactivar esta opción no tendrá efecto en Sherpa.",
        ),
        "row.streaming" => ("Streaming", "Streaming"),
        "stt.selfhost.key.note" => (
            "Bearer token for reverse-proxy setups. Empty means no token.",
            "Token Bearer para proxies inversos. Vacío significa sin token.",
        ),
        "stt.selfhost.stream.note" => (
            "On for ws:// or wss:// servers (sherpa protocol, e.g. our sherpa-online). Off for http(s), which always uses request/response.",
            "Activado para servidores ws:// o wss:// (protocolo sherpa, p. ej. nuestro sherpa-online). Apagado para http(s), que siempre usa petición/respuesta.",
        ),
        "sync.svc.switch" => ("Enable device sync", "Activar sincronización"),
        "sync.svc.advertise" => (
            "Discover this device on the local network",
            "Mostrar este dispositivo en la red local",
        ),
        "sync.svc.note" => (
            "Changes apply automatically while the background service is running. Turning sync off leaves your saved data intact.",
            "Los cambios se aplican automáticamente si el servicio está activo. Desactivar la sincronización conserva los datos guardados.",
        ),
        "home.svc.on" => ("Dictation service running", "Servicio de dictado activo"),
        "home.svc.off" => ("Dictation service stopped", "Servicio de dictado detenido"),
        "home.svc.unhealthy" => (
            "Transcription needs attention",
            "La transcripción necesita atención",
        ),
        "home.hotkeys" => ("Enable shortcuts", "Activar atajos"),
        "row.autodeleteaudio" => ("Auto-delete audio", "Borrado de audio"),
        "row.savediscarded" => (
            "Keep canceled recording entries",
            "Guardar entradas de grabaciones canceladas",
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
        "row.addcombo" => ("New shortcut", "Nuevo atajo"),
        // Dropdown options
        "opt.system" => ("System", "Sistema"),
        "opt.light" => ("Light", "Claro"),
        "opt.dark" => ("Dark", "Oscuro"),
        "opt.hold" => ("Hold", "Mantener"),
        "opt.toggle" => ("Toggle", "Alternar"),
        "opt.smart" => ("Smart", "Inteligente"),
        "opt.bottomcenter" => ("Bottom center", "Abajo centro"),
        "opt.bottomleft" => ("Bottom left", "Abajo izquierda"),
        "opt.bottomright" => ("Bottom right", "Abajo derecha"),
        "opt.sysdefault" => ("System default", "Por defecto del sistema"),
        "opt.builtin" => ("Built-in", "Integrado"),
        "opt.specific" => ("Specific device", "Dispositivo específico"),
        "opt.off" => ("Off", "Desactivado"),
        "opt.ms100" => ("100 ms", "100 ms"),
        "opt.ms250" => ("250 ms", "250 ms"),
        "opt.ms500" => ("500 ms", "500 ms"),
        "opt.ms1000" => ("1 second", "1 segundo"),
        "row.micextra" => ("Extra listening delay", "Escucha extra tras soltar"),
        "opt.hold10s" => ("10 seconds", "10 segundos"),
        "opt.hold60s" => ("60 seconds", "60 segundos"),
        "opt.hold15m" => ("15 minutes", "15 minutos"),
        "opt.local" => ("Local (on-device)", "Local (en el dispositivo)"),
        "opt.cloud" => ("Cloud provider", "Proveedor en la nube"),
        "opt.selfhosted" => ("Self-hosted", "Autohospedado"),
        "opt.ollama" => ("Ollama", "Ollama"),
        "opt.localmodel" => ("Local model", "Modelo local"),
        "opt.localmodel.unavailable" => (
            "Managed local model (unavailable)",
            "Modelo local gestionado (no disponible)",
        ),
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
        "hotkeys.record" => ("Capture keys", "Capturar teclas"),
        "hotkeys.recording" => ("Press keys… (Esc cancels)", "Pulsa teclas… (Esc cancela)"),
        "hotkeys.presskeys" => ("Press keys", "Pulsa teclas"),
        "hotkeys.invalidcombo" => (
            "Enter a valid shortcut, such as Ctrl+Alt+D.",
            "Introduce un atajo válido, como Ctrl+Alt+D.",
        ),
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
        "home.greeting" => ("Dictation", "Dictado"),
        "home.subtitle" => (
            "Speak in any app. Review and copy your recent dictations here.",
            "Habla en cualquier aplicación. Revisa y copia tus dictados recientes aquí.",
        ),
        "home.empty.title" => ("No dictations yet", "Aún no hay dictados"),
        "home.empty.hint" => (
            "Place your cursor in another app, then use {hotkey} to dictate.\nFinished dictations appear here when history is enabled.",
            "Coloca el cursor en otra aplicación y usa {hotkey} para dictar.\nLos dictados terminados aparecen aquí si el historial está activo.",
        ),
        "home.entry.meta" => ("{date} · {words} words", "{date} · {words} palabras"),
        "home.today" => ("Today", "Hoy"),
        "home.yesterday" => ("Yesterday", "Ayer"),
        "home.copy" => ("Copy", "Copiar"),
        "home.delete" => ("Delete", "Eliminar"),
        "home.viewraw" => ("Show original", "Mostrar original"),
        "home.raw.title" => ("Original transcription", "Transcripción original"),
        "home.raw.passthrough" => (
            "The original and final text are identical.",
            "El texto original y el final son idénticos.",
        ),
        "home.wpm" => ("{wpm} wpm", "{wpm} ppm"),
        "home.showdiscarded" => ("Show canceled recordings", "Mostrar grabaciones canceladas"),
        "home.hidediscarded" => ("Hide canceled recordings", "Ocultar grabaciones canceladas"),
        "home.discarded" => ("Recording canceled", "Grabación cancelada"),
        // Insights
        "insights.title" => ("Activity", "Actividad"),
        "insights.subtitle" => (
            "Your saved dictations, calculated on this device. Deleting history also updates these totals.",
            "Tus dictados guardados, calculados en este dispositivo. Al borrar el historial también cambian estos totales.",
        ),
        "insights.words" => ("Words spoken", "Palabras dictadas"),
        "insights.alltime" => ("Saved history", "Historial guardado"),
        "insights.wpm" => ("Words per minute", "Palabras por minuto"),
        "insights.wpm.caption" => (
            "Dictations with a recorded duration",
            "Dictados con duración registrada",
        ),
        "insights.dictations" => ("Dictations", "Dictados"),
        "insights.streak" => ("Current streak", "Racha actual"),
        "insights.streak.value" => ("{n} day{s}", "{n} día{s}"),
        "insights.streak.caption" => ("Consecutive active days", "Días consecutivos con actividad"),
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
        "action.resetdots" => ("Reset app data…", "Restablecer datos…"),
        "action.sendrequest" => ("Send request", "Enviar solicitud"),
        "action.syncnow" => ("Sync now", "Sincronizar ahora"),
        "action.check" => ("Check for updates", "Buscar actualizaciones"),
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
            "Sync complete: {words} words, {snippets} snippets, {history} dictations received from {id}.",
            "Sincronización completa: {words} palabras, {snippets} fragmentos y {history} dictados recibidos de {id}.",
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
        "sync.err.save" => (
            "Could not save this device. Try again.",
            "No se pudo guardar este dispositivo. Inténtalo de nuevo.",
        ),
        "sync.err.badid" => (
            "Enter the other device’s full 64-character hexadecimal ID.",
            "Introduce el ID hexadecimal completo de 64 caracteres del otro dispositivo.",
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
        "sync.reach.off" => (
            "Enable device sync and start the dictation service to connect.",
            "Activa la sincronización e inicia el servicio de dictado para conectar.",
        ),
        "sync.reach.down" => (
            "Sync listener is not answering on port {port}.",
            "El receptor no responde en el puerto {port}.",
        ),
        "sync.reach.up" => (
            "Answering on {port}. LAN peers also need {port}/tcp allowed.",
            "Responde en {port}. Los equipos de la red también necesitan {port}/tcp permitido.",
        ),
        "sync.reach.hint" => ("Allow it with: {cmd}", "Permítelo con: {cmd}"),
        "sync.svc.title" => ("Device sync", "Sincronización de dispositivos"),
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
        "sync.device.id" => ("Device ID", "ID del dispositivo"),
        "sync.device.port" => ("Listen port", "Puerto de escucha"),
        "sync.device.porthint" => (
            "Use a port from 1 to 65535. Set 0 to turn off the listener.",
            "Usa un puerto de 1 a 65535. El valor 0 desactiva el receptor.",
        ),
        "sync.add.title" => ("Add device", "Añadir dispositivo"),
        "sync.add.hint" => (
            "Copy the Device ID from MyWhisper on your other device. Send a pairing request, then accept it there. Both devices must be reachable.",
            "Copia el ID de MyWhisper en tu otro dispositivo. Envía una solicitud y acéptala allí. Ambos dispositivos deben ser accesibles.",
        ),
        "sync.add.idhint" => (
            "Other device’s ID (64 hexadecimal characters)",
            "ID del otro dispositivo (64 caracteres hexadecimales)",
        ),
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
            "No paired devices. Send a request below or accept a request from your other device.",
            "No hay dispositivos emparejados. Envía una solicitud o acepta una de tu otro dispositivo.",
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
        "dict.tab.words" => ("Words", "Palabras"),
        "dict.tab.snippets" => ("Snippets", "Fragmentos"),
        "dict.words.empty" => (
            "No words yet. Add names or specialist terms to guide supported transcription engines. Recognition can still vary.",
            "Aún no hay palabras. Añade nombres o términos especializados para orientar a los motores compatibles. El reconocimiento puede variar.",
        ),
        "dict.words.add" => (
            "For example, MyWhisper or a person’s name",
            "Por ejemplo, MyWhisper o el nombre de una persona",
        ),
        "dict.snippets.empty" => (
            "No snippets yet. Save text and say its trigger phrase during dictation to insert it.",
            "Aún no hay fragmentos. Guarda un texto y di su frase de activación al dictar para insertarlo.",
        ),
        "dict.snippets.trigger" => (
            "For example, my email signature",
            "Por ejemplo, mi firma de correo",
        ),
        "dict.snippets.create" => ("Create snippet", "Crear fragmento"),
        "feedback.saved" => ("Saved", "Guardado"),
        "feedback.deleted" => ("Deleted", "Eliminado"),
        "feedback.copied" => ("Copied to clipboard", "Copiado al portapapeles"),
        "feedback.copyretry" => ("Copy failed. Retry", "Error al copiar. Reintentar"),
        "lm.applying" => ("Applying…", "Aplicando…"),
        "lm.applied" => ("Model settings applied", "Ajustes del modelo aplicados"),
        "lm.applyfailed" => (
            "Could not reach the model. Check its name and server URL.",
            "No se pudo acceder al modelo. Comprueba su nombre y la URL del servidor.",
        ),
        "feedback.copyfailed" => (
            "Could not copy. Check your clipboard tool in Text delivery.",
            "No se pudo copiar. Comprueba la herramienta en Entrega de texto.",
        ),
        "feedback.savefailed" => (
            "Could not save changes. Try again.",
            "No se pudieron guardar los cambios. Inténtalo de nuevo.",
        ),
        "feedback.deletefailed" => (
            "Could not delete. Try again.",
            "No se pudo eliminar. Inténtalo de nuevo.",
        ),
        "feedback.cleared" => ("Saved data cleared", "Datos guardados borrados"),
        "home.history" => ("Recent dictations", "Dictados recientes"),
        "home.search" => ("Search recent dictations", "Buscar dictados recientes"),
        "home.noresults" => ("No matching dictations", "No hay dictados que coincidan"),
        "home.noresults.hint" => (
            "Try another search or clear the search field.",
            "Prueba otra búsqueda o borra el campo.",
        ),
        "home.more" => ("More actions", "Más acciones"),
        "home.hideraw" => ("Hide original", "Ocultar original"),
        "home.setup" => ("Set up dictation", "Configurar dictado"),
        "home.shortcuts" => ("Edit shortcuts", "Editar atajos"),
        "home.privacy" => ("History settings", "Ajustes del historial"),
        "home.historyoff" => ("History is turned off", "El historial está desactivado"),
        "home.historyoff.hint" => (
            "Dictations still reach your apps. Turn on history in Privacy & storage to keep a copy here.",
            "Los dictados siguen llegando a tus aplicaciones. Activa el historial en Privacidad y almacenamiento para guardarlos aquí.",
        ),
        "home.paused" => ("Dictation shortcuts paused", "Atajos de dictado en pausa"),
        "home.start" => ("Start service", "Iniciar servicio"),
        "home.starting" => ("Starting…", "Iniciando…"),
        "home.start.failed" => (
            "Could not start the service. Check that mywhisper.service is installed.",
            "No se pudo iniciar el servicio. Comprueba que mywhisper.service está instalado.",
        ),
        "home.start.ok" => ("Service start requested", "Inicio del servicio solicitado"),
        "home.shortcut.hold" => (
            "Hold {key} to talk. Release to finish.",
            "Mantén {key} para hablar. Suelta para terminar.",
        ),
        "home.shortcut.toggle" => (
            "Press {key} to start. Press again to finish.",
            "Pulsa {key} para empezar. Pulsa de nuevo para terminar.",
        ),
        "home.shortcut.smart" => (
            "Tap {key} to toggle, or hold it to talk.",
            "Toca {key} para alternar o mantén para hablar.",
        ),
        "dict.subtitle" => (
            "Guide recognition with words. Insert saved text with spoken snippets.",
            "Orienta el reconocimiento con palabras. Inserta texto guardado con fragmentos de voz.",
        ),
        "dict.words.hint" => (
            "Add names and terms you use often. Engines with dictionary support use them as hints.",
            "Añade nombres y términos habituales. Los motores compatibles los usan como sugerencias.",
        ),
        "dict.words.label" => ("Word or phrase", "Palabra o frase"),
        "dict.words.create" => ("Add word", "Añadir palabra"),
        "dict.snippets.hint" => (
            "Say a trigger phrase while dictating to insert saved text. Separate alternative triggers with commas.",
            "Di una frase de activación al dictar para insertar texto guardado. Separa las frases alternativas con comas.",
        ),
        "dict.snippets.label" => ("Spoken triggers", "Frases de activación"),
        "dict.snippets.expansion" => ("Text to insert", "Texto que se insertará"),
        "dict.required" => ("Enter a word or phrase.", "Introduce una palabra o frase."),
        "dict.snippets.required" => (
            "Enter both a trigger phrase and the text to insert.",
            "Introduce la frase de activación y el texto que se insertará.",
        ),
        "dict.duplicate" => (
            "This word or trigger already exists. Edit the existing entry instead.",
            "Esta palabra o frase ya existe. Edita la entrada existente.",
        ),
        "confirm.delete.title" => ("Delete this entry?", "¿Eliminar esta entrada?"),
        "confirm.delete.body" => (
            "This cannot be undone. The deletion also applies to devices that sync this data.",
            "No se puede deshacer. El borrado también se aplica a los dispositivos que sincronizan estos datos.",
        ),
        "confirm.audio.title" => (
            "Delete all saved audio?",
            "¿Eliminar todo el audio guardado?",
        ),
        "confirm.audio.body" => (
            "All audio recordings saved on this device will be deleted. Your transcripts will stay. This cannot be undone.",
            "Se borrarán todas las grabaciones guardadas en este dispositivo. Las transcripciones se conservarán. No se puede deshacer.",
        ),
        "confirm.history.title" => (
            "Delete all dictation history?",
            "¿Eliminar todo el historial?",
        ),
        "confirm.history.body" => (
            "Saved transcripts and activity totals will be cleared. The deletion also applies to paired devices when they sync. This cannot be undone.",
            "Se borrarán las transcripciones guardadas y los totales de actividad. El borrado también se aplica a los dispositivos emparejados al sincronizar. No se puede deshacer.",
        ),
        "confirm.cache.title" => (
            "Remove downloaded models?",
            "¿Eliminar los modelos descargados?",
        ),
        "confirm.cache.body" => (
            "Downloaded models and runtimes will be removed. Download them again before using on-device transcription.",
            "Se borrarán los modelos y motores descargados. Descárgalos de nuevo para transcribir en este dispositivo.",
        ),
        "confirm.reset.title" => (
            "Reset MyWhisper data?",
            "¿Restablecer los datos de MyWhisper?",
        ),
        "confirm.reset.body" => (
            "Deletes history, words, snippets, recordings, downloads, and settings on this device. Paired devices and device identity are kept. This cannot be undone.",
            "Elimina historial, palabras, fragmentos, grabaciones, descargas y ajustes de este dispositivo. Se conservan los dispositivos emparejados y la identidad. No se puede deshacer.",
        ),
        "settings.saved" => (
            "Preferences save automatically",
            "Las preferencias se guardan automáticamente",
        ),
        "settings.title" => ("Settings", "Ajustes"),
        "settings.choose" => ("Section", "Sección"),
        "action.moveup" => ("Move earlier", "Subir prioridad"),
        "action.movedown" => ("Move later", "Bajar prioridad"),
        "stt.remote.add" => ("Add remote engine", "Añadir motor remoto"),
        "stt.required" => (
            "Enter a server URL, or a cloud model and API key.",
            "Introduce una URL de servidor o un modelo en la nube y una clave API.",
        ),
        "lm.required" => (
            "Enter a model and a valid server URL, or a cloud model and API key.",
            "Introduce un modelo y una URL válida o un modelo en la nube y una clave API.",
        ),
        "form.badurl" => (
            "Use a complete http://, https://, ws://, or wss:// server URL.",
            "Usa una URL completa con http://, https://, ws:// o wss://.",
        ),
        "sync.title" => ("Devices", "Dispositivos"),
        "sync.working" => ("Syncing…", "Sincronizando…"),
        "sync.remove.title" => (
            "Remove this paired device?",
            "¿Eliminar este dispositivo emparejado?",
        ),
        "sync.remove.body" => (
            "It will stop syncing with this device. Saved words, snippets, and dictations stay on each device.",
            "Dejará de sincronizar con este dispositivo. Las palabras, los fragmentos y los dictados se conservan en cada dispositivo.",
        ),
        "sync.add.idlabel" => ("Other device’s ID", "ID del otro dispositivo"),
        "sync.add.optional" => (
            "Manual address & name (optional)",
            "Dirección manual y nombre (opcionales)",
        ),
        "sync.subtitle" => (
            "Sync words, snippets, and dictation history directly between your trusted devices.",
            "Sincroniza palabras, fragmentos e historial directamente entre tus dispositivos de confianza.",
        ),
        "sync.port.invalid" => (
            "Enter a port between 0 and 65535.",
            "Introduce un puerto entre 0 y 65535.",
        ),
        "sync.self" => (
            "This is your device’s ID. Enter the ID from your other device.",
            "Este es el ID de tu dispositivo. Introduce el ID del otro.",
        ),
        "sync.address.invalid" => (
            "Enter an address as host:port, for example 192.168.1.31:42973.",
            "Introduce una dirección como host:puerto, por ejemplo 192.168.1.31:42973.",
        ),
        "overlay.title" => ("Dictation result", "Resultado del dictado"),
        "overlay.empty" => (
            "Your finished text appears here.",
            "El texto terminado aparece aquí.",
        ),
        "overlay.dismiss" => ("Dismiss", "Cerrar"),
        "overlay.processing" => ("Transcribing…", "Transcribiendo…"),
        "stt.local.current" => (
            "Uses your local model settings below",
            "Usa los ajustes del modelo local de abajo",
        ),
        "confirm.engine.title" => ("Remove this configuration?", "¿Quitar esta configuración?"),
        "confirm.engine.body" => (
            "Its saved connection settings will be removed from the priority list. Downloaded models and dictation history stay on this device.",
            "Sus ajustes de conexión se quitarán de la lista de prioridad. Los modelos descargados y el historial se conservan en este dispositivo.",
        ),
        "confirm.download.title" => ("Delete this download?", "¿Eliminar esta descarga?"),
        "confirm.download.body" => (
            "You will need to download it again to use it. Your settings and dictation history will be kept.",
            "Tendrás que volver a descargarlo para usarlo. Se conservarán tus ajustes y tu historial de dictados.",
        ),
        "backend.caption.0" => ("Fastest, lower quality", "Máxima velocidad, menor calidad"),
        "backend.caption.1" => ("Good balance", "Buen equilibrio"),
        "backend.caption.2" => ("Better quality, slower", "Mejor calidad, más lento"),
        "backend.caption.3" => ("High quality", "Alta calidad"),
        "backend.caption.4" => ("Best quality, slowest", "Máxima calidad, más lento"),
        "backend.caption.5" => ("Fast with good quality", "Rápido y de buena calidad"),
        "backend.caption.6" => (
            "16.9 MB STT, 7 languages, own CPU engine",
            "16,9 MB, 7 idiomas, motor propio para CPU",
        ),
        "backend.caption.7" => (
            "Fast multilingual ASR with auto language detection",
            "Reconocimiento multilingüe rápido con detección automática del idioma",
        ),
        "backend.caption.8" => ("English-only, fast", "Rápido, solo en inglés"),
        "backend.caption.9" => ("English streaming", "Streaming en inglés"),
        "backend.caption.10" => (
            "Multilingual streaming, 15 languages",
            "Streaming multilingüe, 15 idiomas",
        ),
        "backend.caption.11" => (
            "14 languages, no auto-detect (single-language server)",
            "14 idiomas, sin detección automática (un idioma por servidor)",
        ),
        "backend.caption.12" => (
            "Multilingual Parakeet fine-tune (CC-BY-SA-4.0)",
            "Modelo Parakeet ajustado para varios idiomas (CC-BY-SA-4.0)",
        ),
        "backend.caption.13" => (
            "Portable whisper-server, works everywhere",
            "Servidor Whisper portátil para CPU",
        ),
        "backend.caption.14" => (
            "NVIDIA GPUs (Pascal 6.1+, needs proprietary driver)",
            "GPU NVIDIA (Pascal 6.1 o posterior, requiere controlador propietario)",
        ),
        "backend.caption.15" => (
            "AMD/Intel/discrete GPUs via Vulkan",
            "GPU AMD, Intel y dedicadas mediante Vulkan",
        ),
        "backend.caption.16" => (
            "Offline + online websocket servers, CUDA 13 (needs system CUDA libs)",
            "Servidores WebSocket offline y online, CUDA 13 (requiere bibliotecas CUDA)",
        ),
        "backend.caption.17" => (
            "Offline + online websocket servers, onnxruntime included",
            "Servidores WebSocket offline y online, con onnxruntime incluido",
        ),
        "backend.caption.18" => ("CPU server", "Servidor CPU"),
        "backend.caption.19" => ("CUDA server", "Servidor CUDA"),
        "backend.caption.20" => ("Vulkan server", "Servidor Vulkan"),
        "backend.caption.21" => ("Sherpa CUDA server", "Servidor Sherpa CUDA"),
        "backend.caption.22" => ("Sherpa CPU server", "Servidor Sherpa CPU"),
        "lm.residency.default" => ("Ollama default", "Predeterminado de Ollama"),
        "lm.residency.forever" => ("Until server restarts", "Hasta reiniciar el servidor"),
        "lm.residency.unload" => ("Unload immediately", "Descargar de memoria al terminar"),
        "hotkeys.duplicate" => (
            "This shortcut already exists. Change its behavior in the list above.",
            "Este atajo ya existe. Cambia su comportamiento en la lista de arriba.",
        ),
        "hotkeys.keepone" => (
            "Keep at least one shortcut. To pause dictation shortcuts, turn off Enable shortcuts on the Dictation page.",
            "Conserva al menos un atajo. Para pausar los atajos de dictado, desactiva Activar atajos en Dictado.",
        ),
        "engine.keepone" => (
            "Keep at least one configuration. Use its switch to turn it off.",
            "Conserva al menos una configuración. Usa su interruptor para desactivarla.",
        ),
        "server.requested" => (
            "Server request sent to the dictation service",
            "Solicitud de servidor enviada al servicio de dictado",
        ),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn placeholders(text: &str) -> BTreeSet<&str> {
        text.split('{')
            .skip(1)
            .filter_map(|tail| tail.split_once('}').map(|(key, _)| key))
            .collect()
    }

    #[test]
    fn english_and_spanish_have_matching_placeholders() {
        let table = include_str!("i18n.rs")
            .split("fn try_lookup")
            .nth(1)
            .unwrap()
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        for line in table.lines() {
            let Some(line) = line.trim_start().strip_prefix('"') else {
                continue;
            };
            let Some((tag, rest)) = line.split_once('"') else {
                continue;
            };
            if !rest.trim_start().starts_with("=>") {
                continue;
            }
            let (en, es) = try_lookup(tag).unwrap();
            assert!(!en.is_empty() && !es.is_empty(), "{tag}");
            assert_eq!(placeholders(en), placeholders(es), "{tag}");
        }
    }

    #[test]
    fn desktop_ui_literal_translation_tags_exist() {
        for source in [
            include_str!("app.rs"),
            include_str!("app_home.rs"),
            include_str!("app_dictionary.rs"),
            include_str!("app_insights.rs"),
            include_str!("app_settings.rs"),
            include_str!("app_sync.rs"),
            include_str!("overlay.rs"),
        ] {
            for prefix in ["tr(\"", "trf(\""] {
                for tail in source.split(prefix).skip(1) {
                    let tag = tail.split('"').next().unwrap();
                    assert!(try_lookup(tag).is_some(), "Missing UI copy: {tag}");
                }
            }
        }
    }
}
