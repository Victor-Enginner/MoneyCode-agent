// SPDX-License-Identifier: Apache-2.0
//! Ponte de captura e normalização de eventos de spin do sidecar Go.
//! Valida números no intervalo `0..=36`, aplica deduplicação canônica e publica no bus.
//! Desacopla as fontes WS protobuf e MutationObserver do núcleo de decisão.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, error, info, warn};

/// Versão esperada do contrato canônico §5.1.
pub const CANONICAL_EVENT_VERSION: u8 = 1;

/// Endereço padrão do sidecar NDJSON (`127.0.0.1:8787`).
pub const DEFAULT_STREAM_ADDR: &str = "127.0.0.1:8787";

/// Erros do cliente de captura, parser e bus.
#[derive(Debug, Error)]
pub enum CaptureError {
    /// Falha de sintaxe ou campo ausente no JSON canônico.
    #[error("json inválido no evento de spin: {0}")]
    InvalidJson(String),
    /// Versão de contrato incompatível (`v != 1`).
    #[error("versão de evento não suportada: {0}")]
    UnsupportedVersion(u8),
    /// Identificador `spin_id` vazio.
    #[error("spin_id não pode ser vazio")]
    EmptySpinId,
    /// Número fora do intervalo da roleta (`0..=36`).
    #[error("número de roleta inválido (esperado 0..=36): {0}")]
    NumberOutOfRange(i64),
    /// Fonte desconhecida (`source` fora de `ws|mo|ext|ocr`).
    #[error("fonte de captura inválida: {0}")]
    InvalidSource(String),
    /// Erro de I/O na leitura do stream TCP/HTTP.
    #[error("erro de I/O no stream: {0}")]
    Io(#[from] std::io::Error),
    /// Status HTTP inesperado ao conectar em `/stream`.
    #[error("status HTTP inválido do sidecar: {0}")]
    HttpStatus(String),
}

/// Fonte de captura do evento canônico (§5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureSource {
    /// WebSocket protobuf primário.
    Ws,
    /// MutationObserver no frame OOPIF.
    Mo,
    /// Extensão Chrome (`all_frames`).
    Ext,
    /// Reconhecimento óptico (OCR).
    Ocr,
}

impl CaptureSource {
    /// Converte string da especificação §5.1 para `CaptureSource`.
    pub fn parse(raw: &str) -> Result<Self, CaptureError> {
        match raw {
            "ws" => Ok(Self::Ws),
            "mo" => Ok(Self::Mo),
            "ext" => Ok(Self::Ext),
            "ocr" => Ok(Self::Ocr),
            other => Err(CaptureError::InvalidSource(other.to_string())),
        }
    }

    /// Retorna o identificador textual canônico.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ws => "ws",
            Self::Mo => "mo",
            Self::Ext => "ext",
            Self::Ocr => "ocr",
        }
    }
}

/// Evento canônico de spin emitido pelo sidecar (§5.1 do AGENTS.md).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalSpinEvent {
    /// Versão do contrato (`1`).
    pub v: u8,
    /// Identificador monotônico único do spin.
    pub spin_id: String,
    /// Número sorteado validado em `0..=36`.
    pub number: u8,
    /// Timestamp da ocorrência em milissegundos.
    pub ts: i64,
    /// Fonte emissora (`ws|mo|ext|ocr`).
    pub source: CaptureSource,
    /// Latência observada em milissegundos.
    pub latency_ms: i64,
}

/// Faz o parse de uma linha NDJSON do contrato §5.1 e valida `0..=36`.
pub fn parse_spin_event(line: &str) -> Result<CanonicalSpinEvent, CaptureError> {
    let trimmed = line.trim();
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        error!(raw = trimmed, "evento NDJSON malformado");
        return Err(CaptureError::InvalidJson(
            "esperado objeto JSON delimitado por chaves".to_string(),
        ));
    }

    let v_raw = extract_int_field(trimmed, "v")?;
    let v = u8::try_from(v_raw).map_err(|_| CaptureError::UnsupportedVersion(0))?;
    if v != CANONICAL_EVENT_VERSION {
        error!(version = v, "versão de contrato inválida");
        return Err(CaptureError::UnsupportedVersion(v));
    }

    let spin_id = extract_string_field(trimmed, "spin_id")?;
    if spin_id.is_empty() {
        error!("evento sem spin_id");
        return Err(CaptureError::EmptySpinId);
    }

    let number_raw = extract_int_field(trimmed, "number")?;
    if !(0..=36).contains(&number_raw) {
        error!(number = number_raw, "número fora do intervalo 0..=36");
        return Err(CaptureError::NumberOutOfRange(number_raw));
    }
    let number = number_raw as u8;

    let ts = extract_int_field(trimmed, "ts")?;
    let source_str = extract_string_field(trimmed, "source")?;
    let source = CaptureSource::parse(&source_str)?;
    let latency_ms = extract_int_field(trimmed, "latency_ms").unwrap_or(0);

    Ok(CanonicalSpinEvent {
        v,
        spin_id,
        number,
        ts,
        source,
        latency_ms,
    })
}

fn find_field_value<'a>(json: &'a str, key: &str) -> Result<&'a str, CaptureError> {
    let pattern = format!("\"{key}\"");
    let mut search_from = 0;
    while let Some(rel_idx) = json[search_from..].find(&pattern) {
        let idx = search_from + rel_idx;
        let after_key = &json[idx + pattern.len()..].trim_start();
        if let Some(after_colon) = after_key.strip_prefix(':') {
            return Ok(after_colon.trim_start());
        }
        search_from = idx + pattern.len();
    }
    Err(CaptureError::InvalidJson(format!("campo ausente: {key}")))
}

fn extract_string_field(json: &str, key: &str) -> Result<String, CaptureError> {
    let val = find_field_value(json, key)?;
    let rest = val
        .strip_prefix('"')
        .ok_or_else(|| CaptureError::InvalidJson(format!("campo {key} não é string")))?;
    let mut out = String::new();
    let mut escaped = false;
    for ch in rest.chars() {
        if escaped {
            out.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return Ok(out);
        } else {
            out.push(ch);
        }
    }
    Err(CaptureError::InvalidJson(format!(
        "string não terminada no campo {key}"
    )))
}

fn extract_int_field(json: &str, key: &str) -> Result<i64, CaptureError> {
    let val = find_field_value(json, key)?;
    let end = val
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(val.len());
    let num_str = &val[..end];
    num_str.parse::<i64>().map_err(|e| {
        CaptureError::InvalidJson(format!("inteiro inválido no campo {key} ({num_str}): {e}"))
    })
}

/// Deduplicador canônico local (§5.1): filtra por `spin_id` e depois por `(number, ts / 50)`.
#[derive(Debug, Default)]
pub struct Deduplicator {
    seen_ids: HashSet<String>,
    seen_buckets: HashSet<(u8, i64)>,
}

impl Deduplicator {
    /// Cria um novo deduplicador vazio.
    pub fn new() -> Self {
        Self::default()
    }

    /// Retorna `true` se o evento for inédito, registrando suas chaves de idempotência.
    pub fn accept(&mut self, ev: &CanonicalSpinEvent) -> bool {
        if self.seen_ids.contains(&ev.spin_id) {
            debug!(spin_id = %ev.spin_id, "spin descartado por spin_id duplicado");
            return false;
        }
        let bucket = (ev.number, ev.ts / 50);
        if self.seen_buckets.contains(&bucket) {
            debug!(
                spin_id = %ev.spin_id,
                number = ev.number,
                bucket_ts = bucket.1,
                "spin descartado por janela (number, ts/50)"
            );
            return false;
        }
        self.seen_ids.insert(ev.spin_id.clone());
        self.seen_buckets.insert(bucket);
        true
    }
}

/// Barramento local (fanout) para publicar eventos de spin normalizados para o core.
#[derive(Clone, Default)]
pub struct SpinBus {
    subscribers: Arc<Mutex<Vec<Sender<CanonicalSpinEvent>>>>,
}

impl SpinBus {
    /// Cria uma nova instância do barramento de spins.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registra um novo assinante retornando o `Receiver` de eventos canônicos.
    pub fn subscribe(&self) -> Receiver<CanonicalSpinEvent> {
        let (tx, rx) = mpsc::channel();
        if let Ok(mut subs) = self.subscribers.lock() {
            subs.push(tx);
        }
        rx
    }

    /// Emite um evento canônico para todos os assinantes ativos, removendo canais desconectados.
    pub fn emit(&self, event: &CanonicalSpinEvent) -> usize {
        let Ok(mut subs) = self.subscribers.lock() else {
            return 0;
        };
        let mut delivered = 0usize;
        subs.retain(|tx| match tx.send(event.clone()) {
            Ok(()) => {
                delivered += 1;
                true
            }
            Err(_) => false,
        });
        delivered
    }
}

/// Cliente do stream NDJSON do sidecar Go com parser §5.1, validação `0..=36`, dedupe e emissão no bus.
pub struct StreamClient {
    addr: String,
    dedupe: Deduplicator,
    bus: SpinBus,
}

impl StreamClient {
    /// Cria um novo cliente apontando para `addr` (ex.: `"127.0.0.1:8787"`) e associado ao `SpinBus`.
    pub fn new(addr: impl Into<String>, bus: SpinBus) -> Self {
        Self {
            addr: addr.into(),
            dedupe: Deduplicator::new(),
            bus,
        }
    }

    /// Processa uma linha NDJSON bruta: faz o parse, valida `0..=36`, deduplica e emite no bus.
    pub fn ingest_line(&mut self, line: &str) -> Result<Option<CanonicalSpinEvent>, CaptureError> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        let event = parse_spin_event(trimmed)?;
        if !self.dedupe.accept(&event) {
            return Ok(None);
        }
        let delivered = self.bus.emit(&event);
        info!(
            spin_id = %event.spin_id,
            number = event.number,
            source = event.source.as_str(),
            delivered,
            "spin canônico ingerido e publicado no bus"
        );
        Ok(Some(event))
    }

    /// Consome qualquer `BufRead` (socket, pipe ou fixture) até `max_events` (ou EOF se `None`).
    pub fn consume_reader<R: BufRead>(
        &mut self,
        reader: R,
        max_events: Option<usize>,
    ) -> Result< usize, CaptureError> {
        let mut accepted = 0usize;
        for line_res in reader.lines() {
            let line = line_res?;
            let trimmed = line.trim();
            // Ignora linhas de tamanho hexadecimal de chunked encoding se presentes
            if trimmed.is_empty() || !trimmed.starts_with('{') {
                continue;
            }
            match self.ingest_line(trimmed) {
                Ok(Some(_)) => {
                    accepted += 1;
                    if let Some(limit) = max_events {
                        if accepted >= limit {
                            break;
                        }
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    warn!(error = %err, "linha inválida descartada pelo StreamClient");
                }
            }
        }
        Ok(accepted)
    }

    /// Conecta via HTTP em `GET /stream` no sidecar e consome até `max_events` spins válidos.
    pub fn connect_and_consume(&mut self, max_events: Option<usize>) -> Result<usize, CaptureError> {
        let mut stream = TcpStream::connect(&self.addr)?;
        stream.set_read_timeout(Some(Duration::from_secs(90)))?;
        let req = format!(
            "GET /stream HTTP/1.1\r\nHost: {}\r\nAccept: application/x-ndjson\r\nConnection: keep-alive\r\n\r\n",
            self.addr
        );
        stream.write_all(req.as_bytes())?;
        stream.flush()?;

        let mut reader = BufReader::new(stream);
        let mut status_line = String::new();
        reader.read_line(&mut status_line)?;
        if !status_line.contains("200") {
            error!(status = %status_line.trim(), "falha ao abrir /stream no sidecar");
            return Err(CaptureError::HttpStatus(status_line.trim().to_string()));
        }

        // Consome cabeçalhos HTTP até linha em branco
        loop {
            let mut hdr = String::new();
            let n = reader.read_line(&mut hdr)?;
            if n == 0 || hdr == "\r\n" || hdr == "\n" {
                break;
            }
        }

        self.consume_reader(reader, max_events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parse_valid_canonical_spin_event() {
        let raw = r#"{"v":1,"spin_id":"ws-0001","number":17,"ts":1730000000000,"source":"ws","latency_ms":84}"#;
        let ev = parse_spin_event(raw).expect("deve parsear evento canônico §5.1");
        assert_eq!(ev.v, 1);
        assert_eq!(ev.spin_id, "ws-0001");
        assert_eq!(ev.number, 17);
        assert_eq!(ev.ts, 1_730_000_000_000);
        assert_eq!(ev.source, CaptureSource::Ws);
        assert_eq!(ev.latency_ms, 84);
    }

    #[test]
    fn reject_number_out_of_range_0_to_36() {
        let raw_high = r#"{"v":1,"spin_id":"ws-0002","number":37,"ts":1730000000000,"source":"ws","latency_ms":10}"#;
        assert!(matches!(
            parse_spin_event(raw_high),
            Err(CaptureError::NumberOutOfRange(37))
        ));

        let raw_neg = r#"{"v":1,"spin_id":"ws-0003","number":-1,"ts":1730000000000,"source":"mo","latency_ms":10}"#;
        assert!(matches!(
            parse_spin_event(raw_neg),
            Err(CaptureError::NumberOutOfRange(-1))
        ));
    }

    #[test]
    fn deduplicate_by_spin_id_and_time_bucket_and_emit_to_bus() {
        let bus = SpinBus::new();
        let rx = bus.subscribe();
        let mut client = StreamClient::new(DEFAULT_STREAM_ADDR, bus);

        let stream_data = [
            r#"{"v":1,"spin_id":"s-1","number":17,"ts":1730000000010,"source":"ws","latency_ms":40}"#,
            // Mesmo spin_id -> duplicado
            r#"{"v":1,"spin_id":"s-1","number":17,"ts":1730000005000,"source":"mo","latency_ms":90}"#,
            // Outro spin_id mas mesmo (number=17, ts/50 = 34600000000) -> duplicado de cross-source
            r#"{"v":1,"spin_id":"mo-1","number":17,"ts":1730000000040,"source":"mo","latency_ms":70}"#,
            // Novo spin válido
            r#"{"v":1,"spin_id":"s-2","number":0,"ts":1730000030000,"source":"ws","latency_ms":55}"#,
        ]
        .join("\n");

        let count = client
            .consume_reader(Cursor::new(stream_data), None)
            .expect("consumo do stream NDJSON");
        assert_eq!(count, 2);

        let first = rx.try_recv().expect("primeiro evento no bus");
        let second = rx.try_recv().expect("segundo evento no bus");
        assert!(rx.try_recv().is_err(), "duplicatas não devem chegar ao bus");
        assert_eq!(first.number, 17);
        assert_eq!(second.number, 0);
    }
}
