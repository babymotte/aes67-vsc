use sdp::SessionDescription;

pub fn session_id(sdp: &SessionDescription) -> String {
    format!("{} {}", sdp.origin.session_id, sdp.origin.session_version)
}
