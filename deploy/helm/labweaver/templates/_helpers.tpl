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
{{- $policy := required "platformConfigurationPolicy artifact is required when a Bundle is rendered" .Values.platformConfigurationPolicy -}}
{{- if not (kindIs "map" $policy) -}}
{{- fail "platformConfigurationPolicy artifact must be a mapping" -}}
{{- end -}}
{{- $schemaText := .Files.Get "files/platform-configuration-policy-schema.json" -}}
{{- if empty $schemaText -}}
{{- fail "generated Helm values schema is required" -}}
{{- end -}}
{{- $schema := mustFromJson $schemaText -}}
{{- $policySchema := index (index (index $schema "properties") "platformConfigurationPolicy") "oneOf" 1 -}}
{{- $definitions := index $schema "definitions" -}}
{{- range $field := index $policySchema "required" -}}
  {{- if not (hasKey $policy $field) -}}
    {{- fail (printf "platformConfigurationPolicy.%s is required" $field) -}}
  {{- end -}}
{{- end -}}
{{- range $group := list "lifecycle" "runtimePolicy" "builderPolicy" -}}
  {{- if not (kindIs "map" (index $policy $group)) -}}
    {{- fail (printf "platformConfigurationPolicy.%s must be a mapping" $group) -}}
  {{- end -}}
  {{- $groupSchema := index (index $policySchema "properties") $group -}}
  {{- range $required := index $groupSchema "required" -}}
    {{- if not (hasKey (index $policy $group) $required) -}}
      {{- fail (printf "platformConfigurationPolicy.%s.%s is required" $group $required) -}}
    {{- end -}}
  {{- end -}}
{{- end -}}
{{- $lifecycle := index $policy "lifecycle" -}}
{{- $reminders := index $lifecycle "expiryReminderSeconds" -}}
{{- if not (kindIs "slice" $reminders) -}}
{{- fail "platformConfigurationPolicy.lifecycle.expiryReminderSeconds must be a list" -}}
{{- end -}}
{{- if gt (len $reminders) 16 -}}
{{- fail "platformConfigurationPolicy.lifecycle.expiryReminderSeconds has at most 16 entries" -}}
{{- end -}}
{{- $seenReminders := dict -}}
{{- range $value := $reminders -}}
  {{- if not (or (kindIs "int" $value) (kindIs "int64" $value) (kindIs "float64" $value)) -}}
    {{- fail "platformConfigurationPolicy.lifecycle.expiryReminderSeconds entries must be integers" -}}
  {{- end -}}
  {{- $key := printf "%v" $value -}}
  {{- if hasKey $seenReminders $key -}}
    {{- fail "platformConfigurationPolicy.lifecycle.expiryReminderSeconds entries must be unique" -}}
  {{- end -}}
  {{- $_ := set $seenReminders $key true -}}
{{- end -}}
{{- $runtime := index $policy "runtimePolicy" -}}
{{- range $field := list "allowedCapabilities" "preauthorizations" -}}
  {{- if not (kindIs "slice" (index $runtime $field)) -}}
    {{- fail (printf "platformConfigurationPolicy.runtimePolicy.%s must be a list" $field) -}}
  {{- end -}}
{{- end -}}
{{- $defaultUid := index $runtime "defaultContainerUid" -}}
{{- if not (or (kindIs "int" $defaultUid) (kindIs "int64" $defaultUid) (kindIs "float64" $defaultUid)) -}}
{{- fail "platformConfigurationPolicy.runtimePolicy.defaultContainerUid must be an integer" -}}
{{- end -}}
{{- $capEnum := index (index $definitions "LinuxCapability") "enum" -}}
{{- range $capability := index $runtime "allowedCapabilities" -}}
  {{- if not (has $capability $capEnum) -}}
    {{- fail (printf "platformConfigurationPolicy.runtimePolicy.allowedCapabilities contains invalid value at %s" $capability) -}}
  {{- end -}}
{{- end -}}
{{- $preauthSchema := index $definitions "RuntimePreauthorization" -}}
{{- range $entry := index $runtime "preauthorizations" -}}
  {{- if not (kindIs "map" $entry) -}}
    {{- fail "platformConfigurationPolicy.runtimePolicy.preauthorizations entries must be mappings" -}}
  {{- end -}}
  {{- range $required := index $preauthSchema "required" -}}
    {{- if not (hasKey $entry $required) -}}
      {{- fail (printf "platformConfigurationPolicy.runtimePolicy.preauthorizations.%s is required" $required) -}}
    {{- end -}}
  {{- end -}}
  {{- range $key, $_ := $entry -}}
    {{- if not (has $key (list "scope" "capabilities" "privileged")) -}}
      {{- fail (printf "platformConfigurationPolicy.runtimePolicy.preauthorizations.%s is unknown" $key) -}}
    {{- end -}}
  {{- end -}}
  {{- if not (kindIs "slice" (index $entry "capabilities")) -}}
    {{- fail "platformConfigurationPolicy.runtimePolicy.preauthorizations.capabilities must be a list" -}}
  {{- end -}}
  {{- range $capability := index $entry "capabilities" -}}
    {{- if not (has $capability $capEnum) -}}
      {{- fail (printf "platformConfigurationPolicy.runtimePolicy.preauthorizations.capabilities contains invalid value at %s" $capability) -}}
    {{- end -}}
  {{- end -}}
  {{- if not (kindIs "bool" (index $entry "privileged")) -}}
    {{- fail "platformConfigurationPolicy.runtimePolicy.preauthorizations.privileged must be boolean" -}}
  {{- end -}}
  {{- $scope := index $entry "scope" -}}
  {{- if not (kindIs "map" $scope) -}}
    {{- fail "platformConfigurationPolicy.runtimePolicy.preauthorizations.scope must be a mapping" -}}
  {{- else if eq (index $scope "kind") "project" -}}
    {{- if not (hasKey $scope "projectId") -}}
      {{- fail "platformConfigurationPolicy.runtimePolicy.preauthorizations.scope.projectId is required" -}}
    {{- end -}}
  {{- else if eq (index $scope "kind") "course" -}}
    {{- if not (hasKey $scope "courseId") -}}
      {{- fail "platformConfigurationPolicy.runtimePolicy.preauthorizations.scope.courseId is required" -}}
    {{- end -}}
  {{- else -}}
    {{- fail "platformConfigurationPolicy.runtimePolicy.preauthorizations.scope.kind is unsupported" -}}
  {{- end -}}
{{- end -}}
{{- $location := index $runtime "privilegedExecutionLocation" -}}
{{- if not (kindIs "invalid" $location) -}}
  {{- if not (kindIs "map" $location) -}}
    {{- fail "platformConfigurationPolicy.runtimePolicy.privilegedExecutionLocation must be null or a mapping" -}}
  {{- end -}}
  {{- $kind := index $location "kind" -}}
  {{- if not (kindIs "string" $kind) -}}
    {{- fail "platformConfigurationPolicy.runtimePolicy.privilegedExecutionLocation.kind must be a string" -}}
  {{- else if eq $kind "dedicated_node_pool" -}}
    {{- range $required := list "nodeSelector" "taintKey" "taintValue" -}}
      {{- if not (hasKey $location $required) -}}
        {{- fail (printf "platformConfigurationPolicy.runtimePolicy.privilegedExecutionLocation.%s is required" $required) -}}
      {{- end -}}
    {{- end -}}
    {{- if not (kindIs "map" (index $location "nodeSelector")) -}}
      {{- fail "platformConfigurationPolicy.runtimePolicy.privilegedExecutionLocation.nodeSelector must be a mapping" -}}
    {{- end -}}
    {{- range $key, $_ := $location -}}
      {{- if not (has $key (list "kind" "nodeSelector" "taintKey" "taintValue")) -}}
        {{- fail (printf "platformConfigurationPolicy.runtimePolicy.privilegedExecutionLocation.%s is unknown" $key) -}}
      {{- end -}}
    {{- end -}}
  {{- else if eq $kind "virtual_machine_guest" -}}
    {{- if not (hasKey $location "providerBinding") -}}
      {{- fail "platformConfigurationPolicy.runtimePolicy.privilegedExecutionLocation.providerBinding is required" -}}
    {{- end -}}
    {{- range $key, $_ := $location -}}
      {{- if not (has $key (list "kind" "providerBinding")) -}}
        {{- fail (printf "platformConfigurationPolicy.runtimePolicy.privilegedExecutionLocation.%s is unknown" $key) -}}
      {{- end -}}
    {{- end -}}
  {{- else -}}
    {{- fail "platformConfigurationPolicy.runtimePolicy.privilegedExecutionLocation.kind is unsupported" -}}
  {{- end -}}
{{- end -}}
{{- $builder := index $policy "builderPolicy" -}}
{{- if not (kindIs "bool" (index $builder "allowPrivilegeEscalation")) -}}
{{- fail "platformConfigurationPolicy.builderPolicy.allowPrivilegeEscalation must be boolean" -}}
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
