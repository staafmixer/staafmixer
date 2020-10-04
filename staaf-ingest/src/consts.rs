pub const INGEST_PING_PORT: u16 = 8079;
pub const INGEST_PORT: u16 = 8084;

pub const FTL_INGEST_RESP_UNKNOWN: u16 = 0;
pub const FTL_INGEST_RESP_OK: u16 = 200;
pub const FTL_INGEST_RESP_PING: u16 = 201;
/// The handshake was not formatted correctly
pub const FTL_INGEST_RESP_BAD_REQUEST: u16 = 400;
/// This channel id is not authorized to stream
pub const FTL_INGEST_RESP_UNAUTHORIZED: u16 = 401;
/// This ftl api version is no longer supported
pub const FTL_INGEST_RESP_OLD_VERSION: u16 = 402;
pub const FTL_INGEST_RESP_AUDIO_SSRC_COLLISION: u16 = 403;
pub const FTL_INGEST_RESP_VIDEO_SSRC_COLLISION: u16 = 404;
/// The corresponding channel does not match this key
pub const FTL_INGEST_RESP_INVALID_STREAM_KEY: u16 = 405;
/// The channel ID successfully authenticated however it is already actively streaming
pub const FTL_INGEST_RESP_CHANNEL_IN_USE: u16 = 406;
/// Streaming from this country or region is not authorized by local governments
pub const FTL_INGEST_RESP_REGION_UNSUPPORTED: u16 = 407;
pub const FTL_INGEST_RESP_NO_MEDIA_TIMEOUT: u16 = 408;
/// The game the user account is set to can't be streamed.
pub const FTL_INGEST_RESP_GAME_BLOCKED: u16 = 409;
/// The server has terminated the stream.
pub const FTL_INGEST_RESP_SERVER_TERMINATE: u16 = 410;
pub const FTL_INGEST_RESP_INTERNAL_SERVER_ERROR: u16 = 500;
pub const FTL_INGEST_RESP_INTERNAL_MEMORY_ERROR: u16 = 900;
pub const FTL_INGEST_RESP_INTERNAL_COMMAND_ERROR: u16 = 901;
pub const FTL_INGEST_RESP_INTERNAL_SOCKET_CLOSED: u16 = 902;
pub const FTL_INGEST_RESP_INTERNAL_SOCKET_TIMEOUT: u16 = 903;
