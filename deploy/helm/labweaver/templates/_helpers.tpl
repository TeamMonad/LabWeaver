{{- define "labweaver.labels" -}}
app.kubernetes.io/part-of: labweaver
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{- define "labweaver.image" -}}
{{- $image := required (printf "images.%s is required" .key) .value -}}
{{- if not (regexMatch "^[A-Za-z0-9.-]+(:[0-9]{1,5})?/labweaver-system/[a-z0-9-]+@sha256:[0-9a-f]{64}$" $image) -}}
{{- fail (printf "images.%s must be a labweaver-system digest reference" .key) -}}
{{- end -}}
{{- $image -}}
{{- end -}}
