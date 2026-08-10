{{- define "labweaverDemo.labels" -}}
app.kubernetes.io/name: labweaver-demo
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: labweaver
labweaver.io/local-profile: local-hostpath
labweaver.io/owner: labweaver
labweaver.io/evidence-profile: fixture-preview
labweaver.io/release-eligible: "false"
{{- end }}

{{- define "labweaverDemo.image" -}}
{{- required "image.repository is required" .Values.image.repository -}}:{{- required "image.tag is required" .Values.image.tag -}}
{{- end }}
