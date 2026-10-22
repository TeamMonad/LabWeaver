{{- define "labweaver.labels" -}}
app.kubernetes.io/part-of: labweaver
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{- define "labweaver.image" -}}
{{- $image := required (printf "images.%s is required" .key) .value -}}
{{- if not (regexMatch "^[a-z0-9.-]+(?::[0-9]+)?/[a-z0-9._/-]+@sha256:[0-9a-f]{64}$" $image) -}}
{{- fail (printf "images.%s must be an immutable Harbor digest reference" .key) -}}
{{- end -}}
{{- $image -}}
{{- end -}}

{{- define "labweaver.configurationBundleSha256" -}}
{{- $digest := required "deploymentIdentity.configurationBundleSha256 is required" .Values.deploymentIdentity.configurationBundleSha256 -}}
{{- if not (regexMatch "^sha256:[0-9a-f]{64}$" $digest) -}}
{{- fail "deploymentIdentity.configurationBundleSha256 must be a sha256 digest" -}}
{{- end -}}
{{- $digest -}}
{{- end -}}

{{- define "labweaver.controlManagedConfigMap" -}}
{{- printf "%s-control-loaded-%s" .Release.Name (include "labweaver.controlManagedChecksum" . | trunc 20) -}}
{{- end -}}

{{- define "labweaver.controlManagedChecksum" -}}
{{- printf "%s\n%s" (required "managedControlConfiguration.versionJson is required" .Values.managedControlConfiguration.versionJson) (required "managedControlConfiguration.sourceJson is required" .Values.managedControlConfiguration.sourceJson) | sha256sum -}}
{{- end -}}

{{- define "labweaver.platformPolicy" -}}
{{- $policy := required "platformConfigurationPolicy is required when a Bundle is rendered" .Values.platformConfigurationPolicy -}}
{{- if not (kindIs "map" $policy) -}}
{{- fail "platformConfigurationPolicy must be a mapping" -}}
{{- end -}}
{{- range $group := list "lifecycle" "runtimePolicy" "builderPolicy" -}}
  {{- if not (hasKey $policy $group) -}}
    {{- fail (printf "platformConfigurationPolicy.%s is required" $group) -}}
  {{- end -}}
  {{- if not (kindIs "map" (index $policy $group)) -}}
    {{- fail (printf "platformConfigurationPolicy.%s must be a mapping" $group) -}}
  {{- end -}}
{{- end -}}
{{- $runtime := index $policy "runtimePolicy" -}}
{{- if not (hasKey $runtime "privilegedAllowed") -}}
{{- fail "platformConfigurationPolicy.runtimePolicy.privilegedAllowed is required" -}}
{{- end -}}
{{- if not (hasKey $runtime "privilegedExecutionLocation") -}}
{{- fail "platformConfigurationPolicy.runtimePolicy.privilegedExecutionLocation is required" -}}
{{- end -}}
{{- if and (index $runtime "privilegedAllowed") (not (index $runtime "privilegedExecutionLocation")) -}}
{{- fail "platformConfigurationPolicy.runtimePolicy.privilegedExecutionLocation is required when privilegedAllowed is true" -}}
{{- end -}}
{{- $digest := required "platformConfigurationPolicy.configurationSha256 is required" (index $policy "configurationSha256") -}}
{{- if not (regexMatch "^[0-9a-f]{64}$" $digest) -}}
{{- fail "platformConfigurationPolicy.configurationSha256 must be a lowercase SHA-256" -}}
{{- end -}}
{{- toJson $policy -}}
{{- end -}}

{{- define "labweaver.agentControlConnection" -}}
{{- $inputs := .Values.workloads -}}
{{- if .Values.workloadInputs -}}
{{- $inputs = .Values.workloadInputs -}}
{{- end -}}
{{- $environment := (index $inputs "environment-service").env -}}
{{- $control := (index $inputs "control-service").env -}}
{{- $agent := (index $inputs "agent-service").env -}}
{{- $origin := required "Environment Control origin is required for Agent" $environment.LABWEAVER_CONTROL_SERVICE_BASE_URI -}}
{{- $parsed := urlParse $origin -}}
{{- $timeout := required "Environment Control timeout is required for Agent" $environment.LABWEAVER_CONTROL_SERVICE_TIMEOUT_MILLISECONDS -}}
{{- if or (ne $parsed.scheme "https") (empty $parsed.host) (not (has $parsed.path (list "" "/"))) (not (empty $parsed.userinfo)) (not (empty $parsed.query)) (not (empty $parsed.fragment)) (not (regexMatch "^[0-9]+$" $timeout)) (lt (int $timeout) 1) (gt (int $timeout) 5000) -}}
{{- fail "Agent Control connection requires an HTTPS origin and timeout from 1 to 5000ms" -}}
{{- end -}}
{{- if or (ne $environment.LABWEAVER_CONTROL_SERVICE_AUDIENCE $control.LABWEAVER_SERVICE_AUDIENCE) (ne $environment.LABWEAVER_CONTROL_SERVICE_CA_PATH "/etc/labweaver/secrets/mtls-ca.pem") -}}
{{- fail "Agent Control audience/CA must match the existing Control source" -}}
{{- end -}}
{{- $scopes := regexSplit "\\s*,\\s*" $agent.LABWEAVER_SERVICE_SCOPES -1 -}}
{{- $callers := regexSplit "\\s*,\\s*" $control.LABWEAVER_SERVICE_ALLOWED_CLIENT_IDS -1 -}}
{{- if or (not (has "control.agent_run.read" $scopes)) (not (has $agent.LABWEAVER_SERVICE_CLIENT_ID $callers)) -}}
{{- fail "Agent requires its existing Control read role and caller input" -}}
{{- end -}}
{{- $connection := dict -}}
{{- range $key := list "LABWEAVER_CONTROL_SERVICE_BASE_URI" "LABWEAVER_CONTROL_SERVICE_CA_PATH" "LABWEAVER_CONTROL_SERVICE_AUDIENCE" "LABWEAVER_CONTROL_SERVICE_TIMEOUT_MILLISECONDS" -}}
{{- $_ := set $connection $key (index $environment $key) -}}
{{- end -}}
{{- toYaml $connection -}}
{{- end -}}
