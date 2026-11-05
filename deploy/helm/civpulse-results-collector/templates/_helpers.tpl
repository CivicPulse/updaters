{{- define "collector.name" -}}civpulse-results-collector{{- end -}}
{{- define "collector.fullname" -}}{{ printf "%s-%s" .Release.Name (include "collector.name" .) | trunc 63 | trimSuffix "-" }}{{- end -}}
{{- define "collector.labels" -}}
app.kubernetes.io/name: {{ include "collector.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: civpulse
civpulse.org/environment: {{ .Values.environment }}
{{- end -}}
{{- define "collector.selector" -}}
app.kubernetes.io/name: {{ include "collector.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}
{{- define "collector.validate" -}}
{{- if gt (int .Values.collection.maxScopeBytes) (int .Values.collection.maxDownloadBytes) -}}
{{- fail "collection.maxScopeBytes must not exceed maxDownloadBytes" -}}
{{- end -}}
{{- if ne .Release.Namespace (printf "civpulse-%s" .Values.environment) -}}
{{- fail "namespace must be civpulse-<environment>" -}}
{{- end -}}
{{- if not (regexMatch "^sha256:[a-f0-9]{64}$" .Values.image.digest) -}}
{{- fail "image.digest must be an immutable sha256 digest" -}}
{{- end -}}
{{- if .Values.collection.topologyPolicy -}}
{{- if or (not (empty .Values.collection.acceptedTopologies)) (ne .Values.collection.electionDate "2026-11-03") (ne (int .Values.collection.expectedJurisdictions) 160) (ne .Values.collection.sourceUrl "https://results.sos.ga.gov/cdn/results/Georgia/export-11032026GeneralAndSpecialElection.json") -}}
{{- fail "topologyPolicy requires its reviewed source/date/160 scopes and an empty acceptedTopologies" -}}
{{- end -}}
{{- end -}}
{{- if .Values.collection.enabled -}}
{{- if or (empty .Values.collection.electionId) (empty .Values.collection.electionDate) (empty .Values.collection.sourceUrl) (lt (int .Values.collection.expectedItems) 1) -}}
{{- fail "enabled collection requires reviewed election identity/date/URL and expectedItems" -}}
{{- end -}}
{{- if and (eq .Values.environment "prod") (or (not .Values.acceptance.approved) (empty .Values.acceptance.reference)) -}}
{{- fail "production collection requires acceptance.approved and a recorded acceptance.reference" -}}
{{- end -}}
{{- end -}}
{{- end -}}

{{- define "collector.monitorName" -}}
{{ printf "%s-monitor" (include "collector.fullname" . | trunc 55 | trimSuffix "-") }}
{{- end -}}
