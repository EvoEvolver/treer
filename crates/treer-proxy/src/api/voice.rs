use super::*;

pub(super) async fn voice_asr_status(Extension(voice): Extension<VoiceAsrConfig>) -> Json<Value> {
    Json(voice.status_json())
}

pub(super) async fn voice_asr_stream(
    Extension(browser): Extension<BrowserAccess>,
    Extension(voice): Extension<VoiceAsrConfig>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiFailure> {
    browser.validate_if_present(&headers)?;
    if !voice.enabled() {
        return Err(ApiFailure::not_found(
            "voice_asr_unavailable",
            "Voice ASR is unavailable until this Proxy has TREER_VOICE_ASR_PROVIDER=qwen and an API key",
        ));
    }
    Ok(ws.on_upgrade(move |socket| crate::voice::proxy_qwen_asr(socket, voice)))
}

pub(super) async fn voice_command_status(Extension(llm): Extension<VoiceLlmConfig>) -> Json<Value> {
    Json(llm.status_json())
}

#[derive(Debug, Deserialize)]
pub(super) struct VoiceCommandBody {
    text: String,
    #[serde(default)]
    history: Vec<voice_llm::VoiceHistoryTurn>,
}

pub(super) async fn voice_command(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Extension(llm): Extension<VoiceLlmConfig>,
    Path(workspace_id): Path<String>,
    Json(body): Json<VoiceCommandBody>,
) -> Result<Json<Value>, ApiFailure> {
    let result = voice_llm::run_voice_command(voice_llm::VoiceCommandRequest {
        config: &llm,
        state: &state,
        auth: &auth,
        session: &session,
        workspace_id: &workspace_id,
        utterance: &body.text,
        history: &body.history,
    })
    .await
    .map_err(map_voice_command_error)?;
    Ok(Json(result.to_json()))
}

pub(super) fn map_voice_command_error(error: VoiceCommandError) -> ApiFailure {
    match error {
        VoiceCommandError::Unavailable => ApiFailure::not_found(
            "voice_llm_unavailable",
            "Voice command is unavailable until this Proxy has TREER_VOICE_LLM_API_KEY",
        ),
        VoiceCommandError::EmptyUtterance => {
            ApiFailure::bad_request("invalid_utterance", "utterance text is required")
        }
        VoiceCommandError::Upstream(message) => {
            ApiFailure::bad_gateway("voice_llm_failed", &message)
        }
        VoiceCommandError::Protocol(error) => error.into(),
    }
}
