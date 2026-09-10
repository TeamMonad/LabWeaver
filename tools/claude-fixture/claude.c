/*
 * Disposable local-development replacement for the Claude Code executable.
 *
 * This binary is copied only into the explicit agent-runtime-fixture image
 * built by tools/local_dev.py. It implements the stream-json process boundary
 * exercised by agent-service while leaving the production image unchanged.
 */
#include <stdio.h>
#include <string.h>

#ifndef CLAUDE_FIXTURE_VERSION
#define CLAUDE_FIXTURE_VERSION "2.1.215"
#endif

static const char *SESSION_ID = "01900000-0000-7000-8000-000000000701";

static int has_argument(int argc, char **argv, const char *needle) {
    for (int i = 0; i < argc; ++i) {
        if (strstr(argv[i], needle) != NULL) {
            return 1;
        }
    }
    return 0;
}

static void emit_prefix(const char *type, const char *subtype) {
    printf("{\"type\":\"%s\",\"subtype\":\"%s\",\"session_id\":\"%s\"}\n",
           type, subtype, SESSION_ID);
}

static void emit_escaped(const char *value) {
    for (const unsigned char *cursor = (const unsigned char *)value;
         *cursor != '\0'; ++cursor) {
        switch (*cursor) {
        case '"':
            fputs("\\\"", stdout);
            break;
        case '\\':
            fputs("\\\\", stdout);
            break;
        case '\n':
            fputs("\\n", stdout);
            break;
        case '\r':
            fputs("\\r", stdout);
            break;
        case '\t':
            fputs("\\t", stdout);
            break;
        default:
            if (*cursor < 0x20) {
                printf("\\u%04x", *cursor);
            } else {
                fputc(*cursor, stdout);
            }
        }
    }
}

/* Helpers write one JSON object without a trailing newline because the object
 * is the value of the assistant text block below. */
static void emit_environment(int work) {
    const char *klass = work ? "work" : "experiment";
    emit_escaped("{\"apiVersion\":\"environment.labweaver.io/v1\","
                 "\"kind\":\"EnvironmentSpec\",\"name\":\"local-container\","
                 "\"class\":\"");
    emit_escaped(klass);
    emit_escaped("\",\"resources\":{\"cpuMillicores\":1000,\"memoryBytes\":1073741824,"
                 "\"storageBytes\":1073741824},"
                 "\"network\":{\"mode\":\"deny_all\"},"
                 "\"entries\":[{\"name\":\"http\",\"protocol\":\"http\","
                 "\"servicePort\":8080}],"
                 "\"security\":{\"userPolicy\":\"non_root_required\","
                 "\"rootFilesystemPolicy\":\"read_only_required\","
                 "\"privilegeEscalationPolicy\":\"deny\","
                 "\"publicExposurePolicy\":\"deny\","
                 "\"securityProfileBinding\":\"restricted-v1\"},"
                 "\"runtime\":{\"kind\":\"container\","
                 "\"provider_binding\":\"kubernetes-work-local-hostpath\","
                 "\"build_recipe\":{\"mode\":\"generated\",\"files\":[{"
                 "\"path\":\"Dockerfile\",\"content\":\"FROM docker.io/nginxinc/nginx-unprivileged:1.29.5-alpine@sha256:42a7d7f2ee23e9f5a1dcdf3647ba5c585bbd18f79e79cd817e70e8cd61c55779\\n"
                 "USER root\\n"
                 "RUN mkdir -p /opt/labweaver/workspace-seed /workspace /tmp && "
                 "printf '%s' 'This workspace seed is provided by the explicit local integration fixture.' > "
                 "/opt/labweaver/workspace-seed/README.md && "
                 "chmod -R a+rX /opt/labweaver/workspace-seed && "
                 "chmod 0777 /workspace && chmod 1777 /tmp\\n"
                 "USER 65534:65534\\n"
                 "WORKDIR /workspace\\n\"}]},"
                 "\"service_port\":8080},"
                 "\"retention\":{\"policyId\":\"01900000-0000-7000-8000-000000000702\","
                 "\"policyRevision\":1,\"class\":\"run_evidence\","
                 "\"retainUntil\":\"2030-01-01T00:00:00.000Z\","
                 "\"disposition\":\"delete\"}}");
}

static void emit_evaluation(void) {
    /* This is the checked-in Linux evaluation fixture represented as JSON. */
    emit_escaped("{\"apiVersion\":\"evaluation.labweaver.io/v1\","
                 "\"kind\":\"EvaluationSpec\","
                 "\"metadata\":{\"name\":\"linux-local-v1\",\"version\":\"1.0.0\"},"
                 "\"spec\":{"
                 "\"submission\":{\"collector\":{\"kind\":\"system_facts\","
                 "\"facts\":[\"service.nginx.active\"],\"maxBytes\":4194304},"
                 "\"llmReadable\":[]},"
                 "\"steps\":[{\"role\":\"gate\",\"id\":\"probe-preflight\","
                 "\"runner\":{\"kind\":\"ansible_probe\","
                 "\"playbookProfile\":\"linux-nginx-probe-v1\","
                 "\"moduleAllowlist\":[\"ansible.builtin.service_facts\"],"
                 "\"readOnly\":true,\"assertions\":[{\"fact\":\"host.reachable\","
                 "\"expected\":true}]},"
                 "\"checker\":{\"kind\":\"exit_code\",\"expected\":0},"
                 "\"failurePolicy\":\"stop\"}],"
                 "\"aggregation\":{\"kind\":\"deterministic_sum\",\"maxScore\":0,"
                 "\"gates\":[{\"step\":\"probe-preflight\","
                 "\"requiredStatus\":\"passed\"}]},"
                 "\"review\":{\"teacherApprovalRequiredForRelease\":true,"
                 "\"forceManualWhen\":[\"infrastructureError\",\"invalidEvidence\"]}}}");
}

static void emit_work_configuration(void) {
    emit_escaped("{\"scriptContent\":\"#!/bin/sh\\nset -eu\\nprintf '%s\\\\n' configured > /workspace/.labweaver-work-configured\\n\","
                 "\"verificationScriptContent\":\"#!/bin/sh\\nset -eu\\ntest -f /workspace/.labweaver-work-configured\\ngrep -qx configured /workspace/.labweaver-work-configured\\nprintf verified\\n\","
                 "\"summary\":\"Configure the existing Work environment\","
                 "\"requiresRestart\":false}");
}

static void emit_result(void) {
    puts("{\"type\":\"result\",\"subtype\":\"success\","
         "\"is_error\":false,\"session_id\":\"01900000-0000-7000-8000-000000000701\","
         "\"num_turns\":1,\"total_cost_usd\":0,"
         "\"usage\":{\"input_tokens\":1,\"output_tokens\":1},"
         "\"modelUsage\":{\"local-fixture\":{\"inputTokens\":1,\"outputTokens\":1}},"
         "\"permission_denials\":[]}");
}

int main(int argc, char **argv) {
    if (has_argument(argc, argv, "--version")) {
        puts(CLAUDE_FIXTURE_VERSION);
        return 0;
    }

    const int evaluation = has_argument(argc, argv, "EvaluationSpec");
    const int work_configuration = has_argument(argc, argv, "WorkConfigurationDraft");
    const int work_environment = has_argument(argc, argv, "class=work");

    emit_prefix("system", "init");
    emit_prefix("system", "status");
    printf("{\"type\":\"user\",\"session_id\":\"%s\","
           "\"isSynthetic\":true,\"message\":{\"role\":\"user\","
           "\"content\":[{\"type\":\"text\",\"text\":\"local fixture\"}]}}\n",
           SESSION_ID);
    printf("{\"type\":\"assistant\",\"session_id\":\"%s\","
           "\"message\":{\"role\":\"assistant\",\"content\":[{"
           "\"type\":\"thinking\",\"thinking\":\"local fixture\"}]}}\n",
           SESSION_ID);
    printf("{\"type\":\"assistant\",\"session_id\":\"%s\","
           "\"message\":{\"role\":\"assistant\",\"content\":[{"
           "\"type\":\"text\",\"text\":\"",
           SESSION_ID);
    if (work_configuration) {
        emit_work_configuration();
    } else if (evaluation) {
        emit_evaluation();
    } else {
        emit_environment(work_environment);
    }
    puts("\"}]}}");
    emit_result();
    return 0;
}
