# staaf-ingest

Staafmixer ingestion control server.

Build by observation of libftl's code

## Protocol

The control server protocol is rather straightforward

```
> HMAC\r\n\r\n
< 200 <hex encoded message to sign>\n
> CONNECT <channel id in ascii> $<hex encoded signature of message>\r\n\r\n
< 200\n
> ProtocolVersion: 0.9\r\n\r\n
> VendorName: OBS Studio\r\n\r\n
> VendorVersion: 26.0.0\r\n\r\n
> Video: true\r\n\r\n
> VideoCodec: H264\r\n\r\n
> VideoHeight: 720\r\n\r\n
> VideoWidth: 1280\r\n\r\n
> VideoPayloadType: 96\r\n\r\n
> VideoIngestSSRC: 1338\r\n\r\n
> Audio: true\r\n\r\n
> AudioCodec: OPUS\r\n\r\n
> AudioPayloadType: 97\r\n\r\n
> AudioIngestSSRC: 1337\r\n\r\n
> .\r\n\r\n
< 200. Use UDP port 1337\n
```