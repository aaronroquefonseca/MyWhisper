# Captured-resource completion failure

The full suite on the installer-lock correction source (`28aee1c`) reproduced a process-isolated capture-pressure crash. The child printed a passing functional test result and then exited with SIGSEGV. The parent suite correctly failed: 245 passed, one failed, five original opt-in ignores. The failure log is preserved as `suite-before-completion.log`.

The private core for that synthetic child was inspected with gdb. Its main thread was running libc exit/finalization and a dependent library's finalizer. A pool worker was simultaneously in `sqlite3FreeCodecArg` / rusqlite `InnerConnection::close`, destroying a captured encrypted Store through the capture-reader and asset-authority closures. The bundled SQLCipher implementation registers `sqlcipher_extra_shutdown` through an ELF finalizer; its global shutdown must not race active connections.

Four queued drain sentinels proved that the queue had dequeued them. A faster worker can run every sentinel while another worker is still executing or destroying an earlier job. They therefore did not prove that captured Store cleanup had finished before process exit.

The correction tracks actual accepted jobs through captured-resource destruction, including rejection and unwind. A deterministic regression lets later sentinels finish while an earlier body remains blocked, then blocks that job's actual encrypted-Store destructor. Draining remains incomplete at both barriers and completes only after resource destruction is released. The original capture-pressure assertions are unchanged; its cleanup now waits on actual completion. Service timer/settings loops also retain and join their thread owners rather than discarding handles.

The full corrected host and fresh Nix suites pass 250 tests, with the same five opt-in ignores. This proves the specific completion barrier and service-loop ownership corrections. It does not establish full application shutdown across remaining UI/provider producers. That requirement remains explicit under R50.

No core bytes or memory dumps are included in repository evidence.
