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
