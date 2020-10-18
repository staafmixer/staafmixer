import "babel-polyfill";
import {JanusPluginHandle, JanusSession} from "minijanus";

const ws = new WebSocket("ws://localhost:8188", "janus-protocol");
const session = new JanusSession(ws.send.bind(ws));
const handle = new JanusPluginHandle(session);
const conn = new RTCPeerConnection({});
window.conn = conn;

ws.addEventListener("message", ev => session.receive(JSON.parse(ev.data)));
ws.addEventListener("open", async _ => {
    handle.on("message", console.log.bind(console));
    await session.create()
    await handle.attach("eater.staafmixer")

    conn.addEventListener("icecandidate", ev => {
        console.log("ice candidate", ev.candidate);
        handle.sendTrickle(ev.candidate || null).catch(e => console.error("Error trickling ICE: ", e));
    });
    conn.addEventListener("track", e => {
        document.querySelector("video").srcObject = e.streams[0];
    })
    conn.addEventListener("negotiationneeded", _ => {
        console.log("negotiation needed")
        // let offer = conn.createOffer();
        // let local = offer.then(o => conn.setLocalDescription(o));
        // let remote = offer.then(j => handle.sendJsep(j)).then(r => conn.setRemoteDescription(r.jsep));
        Promise.all([local, remote]).catch(e => console.error("Error negotiating offer: ", e));
    });

    await handle.sendMessage({"type": "subscribe", "stream_id": 1337});


    let res = await handle.send("message", {body: {type: "offer"}});
    console.log(res.jsep.sdp);
    await conn.setRemoteDescription(res.jsep);
    let answer = await conn.createAnswer({
        offerToReceiveAudio: true,
        offerToReceiveVideo: true,
    })
    await conn.setLocalDescription(answer);
    console.log(answer.sdp);

    await handle.send("message", {body: {type: "answer"}, jsep: answer});
});