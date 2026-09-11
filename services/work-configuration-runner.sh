#!/bin/sh
# Execute the two files in one Work configuration attempt.
#
# Execution mode arguments are: execution directory, fixed Work directory,
# whether a verification file is required, and the absolute Unix deadline in
# seconds.  The service writes primary.sh and (when requested) verification.sh
# before invoking this file.  User script content is never interpolated into a
# shell command.
#
# The --observe and --cancel modes are the only receipt and process-control
# protocol.  They are intentionally part of this file so the Kubernetes and
# SSH transports cannot grow different PID parsing or signalling rules.
set -u

MAX_OUTPUT_BYTES=65536
CONTROL_LOCK_NAME=.control.lock

write_phase() {
    printf '%s\n' "$2" > "$1/phase" || exit 125
}

fail_before_start() {
    directory=$1
    # A second invocation must not overwrite a receipt produced by the first
    # invocation. The lock is created atomically before any mutable file is
    # touched.
    if [ -d "$directory/.lock" ]; then
        exit 125
    fi
    mkdir -p -- "$directory" 2>/dev/null || exit 125
    printf '%s\n' "$2" > "$directory/runner_error" || exit 125
    printf '%s\n' LW_ENVIRONMENT_WORK_EXECUTION_RUNNER_FAILED >&2
    write_phase "$directory" failed
    exit 125
}

fail_locked() {
    directory=$1
    printf '%s\n' "$2" > "$directory/runner_error" || exit 125
    printf '%s\n' LW_ENVIRONMENT_WORK_EXECUTION_RUNNER_FAILED >&2
    write_phase "$directory" failed
    exit 125
}

fail_mode() {
    printf '%s\n' "$1" >&2
    exit 125
}

valid_directory() {
    case "$1" in
        /*) ;;
        *) return 1 ;;
    esac
    case "$1" in
        *..*|*//*|*[![:print:]]*) return 1 ;;
    esac
    return 0
}

require_tools() {
    for tool in "$@"; do
        command -v "$tool" >/dev/null 2>&1 || return 1
    done
    return 0
}

valid_exit_code() {
    value=$1
    case "$value" in
        -*) digits=${value#-} ;;
        *) digits=$value ;;
    esac
    case "$digits" in
        ''|*[!0-9]*) return 1 ;;
        *) return 0 ;;
    esac
}

# The directory is a short-lived mutex.  It is held only while deciding
# whether a process may be launched and while recording its PID.  Cancellation
# takes the same mutex before writing its marker and inspecting the PID, which
# closes the pre-PID race between a durable cancel and runner launch.
acquire_control() {
    directory=$1
    attempts=0
    while ! mkdir "$directory/$CONTROL_LOCK_NAME" 2>/dev/null; do
        attempts=$((attempts + 1))
        [ "$attempts" -lt 200 ] || return 1
        sleep 0.05 || return 1
    done
    return 0
}

release_control() {
    rmdir "$1/$CONTROL_LOCK_NAME" 2>/dev/null || return 1
    return 0
}

read_process_identity() {
    pid=$1
    [ -r "/proc/$pid/stat" ] || return 1
    awk '{ line=$0; sub(/^.*\) /, "", line); n=split(line, fields, /[[:space:]]+/); if (n < 20) exit 1; print fields[20], fields[3], fields[4] }' "/proc/$pid/stat"
}

observe_mode() {
    directory=$1
    verification_required=$2
    valid_directory "$directory" || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_RUNNER_ARGUMENT_INVALID
    case "$verification_required" in
        0|1) ;;
        *) fail_mode LW_ENVIRONMENT_WORK_EXECUTION_RUNNER_ARGUMENT_INVALID ;;
    esac
    require_tools cat || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_TOOL_MISSING

    [ -f "$directory/phase" ] || exit 3
    phase=$(cat -- "$directory/phase") || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_OBSERVATION_INVALID
    case "$phase" in
        done) ;;
        failed)
            # Failed before the runner produced a receipt.  Keep the error
            # payload out of the transport protocol; it is a stable diagnostic
            # written by the runner and is surfaced as RunnerFailed by callers.
            printf 'LW_ENVIRONMENT_WORK_EXECUTION_RUNNER_FAILED\n' >&2
            exit 125
            ;;
        *) exit 3 ;;
    esac
    # A runner error takes precedence even when the phase was finally closed.
    # For example, an output collector timeout writes the exit file and then
    # closes the phase as done.
    if [ -s "$directory/runner_error" ]; then
        printf 'LW_ENVIRONMENT_WORK_EXECUTION_RUNNER_FAILED\n' >&2
        exit 125
    fi

    [ -f "$directory/primary_exit" ] || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_OBSERVATION_INVALID
    primary=$(cat -- "$directory/primary_exit") || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_OBSERVATION_INVALID
    valid_exit_code "$primary" || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_OBSERVATION_INVALID

    verification=-
    # A non-zero primary exit means the verification script was deliberately
    # skipped.  Its absence is therefore a valid terminal receipt even when a
    # verification script was requested.  Only a successful primary run must
    # have a verification result when verification is required.
    if [ "$primary" -eq 0 ] && [ "$verification_required" = 1 ]; then
        [ -f "$directory/verification_exit" ] || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_VERIFICATION_MISSING
        verification=$(cat -- "$directory/verification_exit") || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_OBSERVATION_INVALID
        valid_exit_code "$verification" || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_OBSERVATION_INVALID
    elif [ "$primary" -eq 0 ] && [ -e "$directory/verification_exit" ]; then
        # There is no verification result in a no-verification request.  A
        # successful primary run with an unexpected file is an invalid receipt.
        fail_mode LW_ENVIRONMENT_WORK_EXECUTION_OBSERVATION_INVALID
    fi

    truncated=0
    if [ -e "$directory/output_truncated" ]; then
        marker=$(cat -- "$directory/output_truncated") || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_OBSERVATION_INVALID
        [ "$marker" = true ] || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_OBSERVATION_INVALID
        truncated=1
    fi
    [ -f "$directory/output" ] || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_OBSERVATION_INVALID
    printf '%s\n%s\n%s\n' "$primary" "$verification" "$truncated"
    cat -- "$directory/output" || exit 125
}

cancel_mode() {
    directory=$1
    valid_directory "$directory" || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_RUNNER_ARGUMENT_INVALID
    require_tools awk cat kill sleep mkdir rmdir || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_TOOL_MISSING
    mkdir -p -- "$directory" 2>/dev/null || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_CANCELLATION_FAILED
    acquire_control "$directory" || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_CANCELLATION_TIMEOUT

    # This marker is durable and is checked by the runner while holding the
    # same control lock.  It therefore prevents a process from starting after
    # cancellation wins the database start fence, even when no PID exists yet.
    : > "$directory/cancel" || {
        release_control "$directory" >/dev/null 2>&1 || :
        fail_mode LW_ENVIRONMENT_WORK_EXECUTION_CANCELLATION_FAILED
    }
    if [ ! -f "$directory/pid" ]; then
        release_control "$directory" >/dev/null 2>&1 || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_CANCELLATION_FAILED
        exit 0
    fi

    pid= start=
    if ! read pid start < "$directory/pid"; then
        release_control "$directory" >/dev/null 2>&1 || :
        fail_mode LW_ENVIRONMENT_WORK_EXECUTION_CANCELLATION_FAILED
    fi
    case "$pid:$start" in
        ''|*[!0-9:]*|0:*|*:0) release_control "$directory" >/dev/null 2>&1 || :; fail_mode LW_ENVIRONMENT_WORK_EXECUTION_CANCELLATION_FAILED ;;
    esac
    identity=$(read_process_identity "$pid" 2>/dev/null) || {
        # The process exited before the signal.  The marker remains and the
        # runner or observer will close the durable receipt.
        release_control "$directory" >/dev/null 2>&1 || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_CANCELLATION_FAILED
        exit 0
    }
    set -- $identity
    if [ "$#" -ne 3 ] || [ "$1" != "$start" ] || [ "$2" != "$pid" ] || [ "$3" != "$pid" ]; then
        release_control "$directory" >/dev/null 2>&1 || :
        fail_mode LW_ENVIRONMENT_WORK_EXECUTION_TARGET_IDENTITY_CHANGED
    fi
    if ! kill -TERM "-$pid" 2>/dev/null; then
        if [ -r "/proc/$pid/stat" ]; then
            release_control "$directory" >/dev/null 2>&1 || :
            fail_mode LW_ENVIRONMENT_WORK_EXECUTION_CANCELLATION_FAILED
        fi
        release_control "$directory" >/dev/null 2>&1 || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_CANCELLATION_FAILED
        exit 0
    fi
    sleep 1 || {
        release_control "$directory" >/dev/null 2>&1 || :
        fail_mode LW_ENVIRONMENT_WORK_EXECUTION_CANCELLATION_FAILED
    }
    identity=$(read_process_identity "$pid" 2>/dev/null) || {
        release_control "$directory" >/dev/null 2>&1 || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_CANCELLATION_FAILED
        exit 0
    }
    set -- $identity
    if [ "$#" -eq 3 ] && [ "$1" = "$start" ] && [ "$2" = "$pid" ] && [ "$3" = "$pid" ] && kill -0 "$pid" 2>/dev/null; then
        kill -KILL "-$pid" 2>/dev/null || {
            if [ -r "/proc/$pid/stat" ]; then
                release_control "$directory" >/dev/null 2>&1 || :
                fail_mode LW_ENVIRONMENT_WORK_EXECUTION_CANCELLATION_FAILED
            fi
        }
    fi
    release_control "$directory" >/dev/null 2>&1 || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_CANCELLATION_FAILED
    exit 0
}

capture_output() {
    directory=$1
    append=$2
    # Keep one read descriptor open for both reads. Reopening a FIFO after
    # head exits can wait forever when the producer has already closed it.
    exec 3< "$directory/output.pipe" || return 1
    output_bytes=$(wc -c < "$directory/output") || return 1
    case "$output_bytes" in
        ''|*[!0-9]*) return 1 ;;
    esac
    if [ "$output_bytes" -ge "$MAX_OUTPUT_BYTES" ]; then
        cat <&3 > /dev/null || return 1
        printf 'true\n' > "$directory/output_truncated" || return 1
        exec 3<&-
        return 0
    fi
    remaining=$((MAX_OUTPUT_BYTES - output_bytes))
    if [ "$append" = 1 ]; then
        head -c "$remaining" <&3 >> "$directory/output" || return 1
    else
        head -c "$remaining" <&3 > "$directory/output" || return 1
    fi
    overflow=$(wc -c <&3) || return 1
    case "$overflow" in
        ''|*[!0-9]*) return 1 ;;
    esac
    if [ "$overflow" -gt 0 ]; then
        printf 'true\n' > "$directory/output_truncated" || return 1
    fi
    exec 3<&-
    return 0
}

# Internal mode used to run the collector under the same absolute deadline
# plus a short drain grace period. It keeps escaped descendants from holding
# the main runner forever after the user process has timed out.
if [ "${1:-}" = --capture ] && [ "$#" -eq 3 ]; then
    capture_output "$2" "$3"
    exit $?
fi

if [ "${1:-}" = --observe ]; then
    [ "$#" -eq 3 ] || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_RUNNER_ARGUMENT_INVALID
    observe_mode "$2" "$3"
    exit $?
fi

if [ "${1:-}" = --cancel ]; then
    [ "$#" -eq 2 ] || fail_mode LW_ENVIRONMENT_WORK_EXECUTION_RUNNER_ARGUMENT_INVALID
    cancel_mode "$2"
    exit $?
fi

if [ "$#" -ne 4 ]; then
    exit 64
fi
execution_directory=$1
work_directory=$2
verification_required=$3
deadline_seconds=$4

if ! valid_directory "$execution_directory"; then
    fail_before_start "$execution_directory" LW_ENVIRONMENT_WORK_EXECUTION_RUNNER_ARGUMENT_INVALID
fi
case "$verification_required" in
    0|1) ;;
    *) fail_before_start "$execution_directory" LW_ENVIRONMENT_WORK_EXECUTION_RUNNER_ARGUMENT_INVALID ;;
esac
case "$deadline_seconds" in
    ''|*[!0-9]*|0) fail_before_start "$execution_directory" LW_ENVIRONMENT_WORK_EXECUTION_DEADLINE_INVALID ;;
esac
case "$work_directory" in
    /*) ;;
    *) fail_before_start "$execution_directory" LW_ENVIRONMENT_WORK_EXECUTION_WORKDIR_INVALID ;;
esac
case "$work_directory" in
    *..*|*//*|*[![:print:]]*) fail_before_start "$execution_directory" LW_ENVIRONMENT_WORK_EXECUTION_WORKDIR_INVALID ;;
esac

if ! require_tools date mkdir mkfifo setsid awk head wc cat rm rmdir sleep kill timeout grep; then
    fail_before_start "$execution_directory" LW_ENVIRONMENT_WORK_EXECUTION_TOOL_MISSING
fi
timeout --version 2>&1 | grep -q 'GNU coreutils' || fail_before_start "$execution_directory" LW_ENVIRONMENT_WORK_EXECUTION_TOOL_MISSING

mkdir -p -- "$execution_directory" || exit 125
# This mkdir is the execution lock. Never remove it: a stale or interrupted
# attempt is reconciled from its receipt rather than rerun.
mkdir "$execution_directory/.lock" 2>/dev/null || exit 125
[ -f "$execution_directory/primary.sh" ] || fail_locked "$execution_directory" LW_ENVIRONMENT_WORK_EXECUTION_PRIMARY_SCRIPT_MISSING
if [ "$verification_required" = 1 ]; then
    [ -f "$execution_directory/verification.sh" ] || fail_locked "$execution_directory" LW_ENVIRONMENT_WORK_EXECUTION_VERIFICATION_SCRIPT_MISSING
fi
: > "$execution_directory/output" || exit 125
rm -f -- "$execution_directory/runner_error" "$execution_directory/output_truncated" "$execution_directory/verification_exit" "$execution_directory/timeout" || exit 125

# Handle a cancellation that won before the runner was invoked.  The check is
# serialized with --cancel, so the marker cannot appear immediately after this
# decision and before the first process launch.
if ! acquire_control "$execution_directory"; then
    fail_locked "$execution_directory" LW_ENVIRONMENT_WORK_EXECUTION_CANCELLATION_TIMEOUT
fi
if [ -f "$execution_directory/cancel" ]; then
    printf '143\n' > "$execution_directory/primary_exit" || exit 125
    release_control "$execution_directory" || exit 125
    write_phase "$execution_directory" done
    exit 0
fi
release_control "$execution_directory" || fail_locked "$execution_directory" LW_ENVIRONMENT_WORK_EXECUTION_CANCELLATION_FAILED

run_script() {
    execution_directory=$1
    work_directory=$2
    script_file=$3
    exit_file=$4
    phase=$5
    deadline_seconds=$6

    write_phase "$execution_directory" "$phase"
    now=$(date +%s) || fail_locked "$execution_directory" LW_ENVIRONMENT_WORK_EXECUTION_CLOCK_FAILED
    remaining=$((deadline_seconds - now))
    if [ "$remaining" -le 0 ]; then
        printf 'true\n' > "$execution_directory/timeout" || fail_locked "$execution_directory" LW_ENVIRONMENT_WORK_EXECUTION_WRITE_FAILED
        printf '124\n' > "$execution_directory/$exit_file" || fail_locked "$execution_directory" LW_ENVIRONMENT_WORK_EXECUTION_WRITE_FAILED
        return 0
    fi

    if ! acquire_control "$execution_directory"; then
        fail_locked "$execution_directory" LW_ENVIRONMENT_WORK_EXECUTION_CANCELLATION_TIMEOUT
    fi
    if [ -f "$execution_directory/cancel" ]; then
        printf '143\n' > "$execution_directory/$exit_file" || exit 125
        release_control "$execution_directory" || exit 125
        return 0
    fi
    rm -f -- "$execution_directory/output.pipe" || fail_locked "$execution_directory" LW_ENVIRONMENT_WORK_EXECUTION_WRITE_FAILED
    mkfifo "$execution_directory/output.pipe" || fail_locked "$execution_directory" LW_ENVIRONMENT_WORK_EXECUTION_WRITE_FAILED
    collector_seconds=$((remaining + 2))
    timeout --signal=TERM --kill-after=1s "$collector_seconds"         /bin/sh "$0" --capture "$execution_directory" 1 &
    capture_pid=$!

    # setsid makes this process the leader of a private session/process group.
    # The shell keeps that PID when it execs timeout, so the persisted PID is
    # also the group leader used by cancellation and the deadline timeout.
    setsid /bin/sh -c '
        if [ -f "$1/cancel" ]; then exit 143; fi
        pid=$$
        start=$(awk '\''{ line=$0; sub(/^.*\) /, "", line); n=split(line, fields, /[[:space:]]+/); if (n < 20) exit 1; print fields[20] }'\'' /proc/$pid/stat) || exit 127
        if [ -f "$1/cancel" ]; then exit 143; fi
        printf '\''%s %s\n'\'' "$pid" "$start" > "$1/pid" || exit 125
        cd -- "$3" || exit 126
        if [ -f "$1/cancel" ]; then exit 143; fi
        exec timeout --signal=TERM --kill-after=1s "$4" /bin/sh "$2"
    ' labweaver-work "$execution_directory" "$script_file" "$work_directory" "$remaining"         > "$execution_directory/output.pipe" 2>&1 &
    script_pid=$!

    # Do not release the cancellation mutex until the PID has been persisted
    # or the launch has definitely failed.  The child also checks the marker
    # before exec, making the check-and-launch decision atomic with cancel.
    launch_attempts=0
    while [ ! -f "$execution_directory/pid" ] && [ "$launch_attempts" -lt 100 ]; do
        launch_attempts=$((launch_attempts + 1))
        sleep 0.01 || exit 125
    done
    release_control "$execution_directory" || fail_locked "$execution_directory" LW_ENVIRONMENT_WORK_EXECUTION_CANCELLATION_FAILED

    wait "$script_pid"
    status=$?
    wait "$capture_pid"
    capture_status=$?
    rm -f -- "$execution_directory/output.pipe" "$execution_directory/pid" || fail_locked "$execution_directory" LW_ENVIRONMENT_WORK_EXECUTION_WRITE_FAILED
    if [ "$capture_status" -ne 0 ]; then
        printf 'LW_ENVIRONMENT_WORK_EXECUTION_OUTPUT_COLLECTOR_TIMEOUT\n' > "$execution_directory/runner_error" || exit 125
        status=125
    fi
    printf '%s\n' "$status" > "$execution_directory/$exit_file" || fail_locked "$execution_directory" LW_ENVIRONMENT_WORK_EXECUTION_WRITE_FAILED
}

run_script "$execution_directory" "$work_directory" "$execution_directory/primary.sh" primary_exit primary "$deadline_seconds"
primary_status=$(cat "$execution_directory/primary_exit") || fail_locked "$execution_directory" LW_ENVIRONMENT_WORK_EXECUTION_OBSERVATION_INVALID
case "$primary_status" in
    ''|*[!0-9-]*) fail_locked "$execution_directory" LW_ENVIRONMENT_WORK_EXECUTION_OBSERVATION_INVALID ;;
esac
if [ "$verification_required" = 1 ] && [ "$primary_status" -eq 0 ]; then
    run_script "$execution_directory" "$work_directory" "$execution_directory/verification.sh" verification_exit verification "$deadline_seconds"
fi
write_phase "$execution_directory" done
exit 0
