# Adopt an existing Linux service

`adopt-existing` adds Observatory's retained application-update baseline to an
older installation without replacing its authentication, address, data, support
files or original service definition. It is an explicit local owner operation;
it does not enroll an Instance service or require a shared provisioning service.

The initial supported case is the original, unchanged Linux **user systemd** unit
created by Observatory's source installer: a regular unit file, `Type=simple`,
`Restart=on-failure`, and `KillMode=control-group`, with the original plain `serve`
command. Modified units, drop-ins, other platforms and linked unit definitions
are refused. Configured HTTPS certificate, key and optional CA files must be
regular canonical files inside the existing private home's `tls` directory.
External or linked TLS files are not repaired or adopted by this path.

The source installer has written two exact texts for that unit. Installers
through 0.12.2 wrote it without, and later installers write it with, the line
`RestartForceExitStatus=SIGTERM` directly after `Restart=on-failure`. That line
restarts the service after another program ends it with a termination signal; a
stop through the service manager and the application's own clean exit still
leave it stopped. Each text is accepted only byte for byte, and a unit with any
other difference is refused. Adoption and later application updates never
rewrite the unit, and an adopted installation stays bound to the exact text it
was adopted with.

This path requires the existing `/usr/bin/systemctl` and `/usr/bin/busctl`
tools and the current user's private runtime directory at `/run/user/<uid>`.
The manager is identified through the canonical local user bus, with its process
generation, bus identity and connection credentials checked together. Conflicting
`XDG_RUNTIME_DIR` or `DBUS_SESSION_BUS_ADDRESS` settings refuse adoption rather
than selecting another manager. No permission repair or manager executable-link
access is required; the application executable checks remain separate.

## Before adoption

The owner first reviews unsettled effects and performs the original service's
maintenance stop through the service manager (`systemctl --user stop`). With the
newer unit text, a main process ended directly by a termination signal is
started again. An idle count or a successful stop acknowledgement alone is
insufficient: previously accepted management work may have effects outside the
process, and a child can outlive a directly terminated parent. Account for those
effects and preserve historical unknown results; do not replay them or mark them
resolved merely to complete adoption.

The adoption command independently requires the exact original unit to be
inactive, with no pending manager job, captured process generations, cgroup
members or listening endpoint. It takes the original `service.lock` without
replacing its inode, then checks the raw database before opening application
state. A failed or auto-restarting unit is not treated as inactive. Resolve that
state through separate owner maintenance; adoption does not reset failures,
rewrite the unit or issue an alternative stop.

Use a complete matching candidate package. Read-only inspection can supply some
of the bindings needed for the private review:

```sh
/path/to/download/agent-observatory --home /absolute/existing/home adopt-existing --inspect
```

Inspection reports hashes, lock identity and hashes of durable running-record
keys. It does not stop the service, establish an admission fence, prove external
effects settled, or authorize adoption. Keep its output and the owner review
private.

The owner-only review JSON uses the exact [`Review` schema](../src/desktop_update/managed/linux.rs).
It binds the request UUID, installation/home, current user, old and candidate
executable hashes, original version/configuration/raw-state hashes, original lock
identity, original manager definition and generation, captured process
generations, and the copied-state review. Its effects section explicitly accounts
for the durable running records and preserves unknowns without replay. These are
owner declarations backed by retained evidence, not computer proof that remote
effects have settled. Do not manufacture missing hashes or manager observations.

## Adopt and reconcile

```sh
/path/to/download/agent-observatory --home /absolute/existing/home adopt-existing --review /private/review.json
```

The command preserves the observed legacy origin rather than inventing a previous
package baseline. It stages the candidate, replaces only the application
executable, and starts it through the **same original unit**. The new process
checks the saved intent and manager generation before opening state. Admission
stays closed until that exact process is verified and the new baseline is
published. Existing startup recovery may normalize interrupted records; adoption
does not roll back the database or replay work.

If the result is interrupted or unknown, retain the same request and evidence:

```sh
/path/to/download/agent-observatory --home /absolute/existing/home adopt-existing --request-id SAVED_UUID
```

This reconciles the saved operation; it does not repeat an uncertain Start or
Stop. A failed candidate can be explicitly recovered with the same request:

```sh
/path/to/download/agent-observatory --home /absolute/existing/home adopt-existing --request-id SAVED_UUID --recover
```

Recovery must prove the candidate absent before restoring the exact previous
executable and starting it through the original unit. Unknown or refused manager
operations may require separate owner maintenance first. A published successful
candidate is settled forward rather than silently replaced. Recovery is not a
command to roll back an already verified successful update.

Later normal application updates retain the same manager binding and use the
existing application-update controls. A complete, correlated pre-shutdown Busy
reply preserves current work and permits a later **explicit** apply for the same
unchanged generation; its attempt history remains saved. A lost or ambiguous
reply never permits automatic redispatch. A lost final acknowledgement can be
reconciled against the verified generation without another launch.

This source path has focused local checks. Actual disposable manager adoption,
recovery and subsequent-update evidence is separate; this document makes no
live-installation or cross-platform adoption claim.
