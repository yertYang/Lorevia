"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { apiUrl, requestJson } from "@/lib/api";
import { stripAudioMimeParameters } from "@/lib/voice-mime";
import { activeWorkspaceId } from "@/lib/workspace-scope";

export type RecorderState = "idle" | "checking" | "recording" | "transcribing";

/**
 * Microphone capture → backend transcription. Records via MediaRecorder, posts
 * the clip to ``/api/voice/stt`` (which uses the selected STT model or server
 * default), and hands the transcript back through ``onTranscript``.
 */
export function useVoiceRecorder(onTranscript: (text: string) => void) {
  const { t } = useTranslation();
  const [state, setState] = useState<RecorderState>("idle");
  const [error, setError] = useState<string | null>(null);
  const recorderRef = useRef<MediaRecorder | null>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const requestRef = useRef<AbortController | null>(null);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const onTranscriptRef = useRef(onTranscript);
  onTranscriptRef.current = onTranscript;

  const releaseStream = useCallback(() => {
    streamRef.current?.getTracks().forEach((track) => track.stop());
    streamRef.current = null;
  }, []);

  const start = useCallback(async () => {
    if (state !== "idle" || requestRef.current) return;
    setError(null);
    if (
      typeof navigator === "undefined" ||
      !navigator.mediaDevices?.getUserMedia ||
      typeof MediaRecorder === "undefined"
    ) {
      setError(t("Recording is not supported in this browser."));
      return;
    }
    const controller = new AbortController();
    requestRef.current = controller;
    const workspace = activeWorkspaceId();
    const url = apiUrl("/api/voice/stt");
    setState("checking");
    try {
      const { stt } = await requestJson<{
        stt: { configured: boolean; message?: string };
      }>(apiUrl("/api/voice/status"), {
        signal: AbortSignal.any([
          controller.signal,
          AbortSignal.timeout(15000),
        ]),
      });
      if (!stt.configured) {
        throw new Error(
          stt.message || "Choose a speech-to-text model in Settings → Voice and set it as default before recording.",
        );
      }
    } catch (err) {
      if (!controller.signal.aborted) {
        setError(
          t(err instanceof Error ? err.message : "Transcription failed"),
        );
        setState("idle");
      }
      if (requestRef.current === controller) requestRef.current = null;
      return;
    }
    let stream: MediaStream;
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    } catch {
      if (!controller.signal.aborted) {
        setError(t("Microphone permission denied."));
        setState("idle");
      }
      if (requestRef.current === controller) requestRef.current = null;
      return;
    }
    if (controller.signal.aborted || activeWorkspaceId() !== workspace) {
      stream.getTracks().forEach((track) => track.stop());
      if (requestRef.current === controller) {
        requestRef.current = null;
        setState("idle");
      }
      return;
    }
    streamRef.current = stream;
    let recorder: MediaRecorder;
    try {
      recorder = new MediaRecorder(stream);
    } catch {
      releaseStream();
      requestRef.current = null;
      setState("idle");
      setError(t("Recording is not supported in this browser."));
      return;
    }
    const chunks: Blob[] = [];
    let bytes = 0;
    let recordingTimer: ReturnType<typeof setTimeout> | null = null;
    const clearRecordingTimer = () => {
      if (recordingTimer) clearTimeout(recordingTimer);
      if (timerRef.current === recordingTimer) timerRef.current = null;
    };
    recorder.ondataavailable = (event) => {
      if (controller.signal.aborted) return;
      if (event.data && event.data.size > 0) {
        bytes += event.data.size;
        if (bytes > 25 * 1024 * 1024) {
          controller.abort();
          if (recorder.state !== "inactive") recorder.stop();
          releaseStream();
          requestRef.current = null;
          setState("idle");
          setError(t("Recordings must be at most 25 MiB."));
        } else chunks.push(event.data);
      }
    };
    recorder.onstop = async () => {
      clearRecordingTimer();
      const mimeType = stripAudioMimeParameters(recorder.mimeType);
      stream.getTracks().forEach((track) => track.stop());
      if (streamRef.current === stream) streamRef.current = null;
      if (recorderRef.current === recorder) recorderRef.current = null;
      if (controller.signal.aborted || activeWorkspaceId() !== workspace) {
        if (requestRef.current === controller) {
          requestRef.current = null;
          setState("idle");
        }
        return;
      }
      const blob = new Blob(chunks, { type: mimeType });
      if (!blob.size) {
        requestRef.current = null;
        setState("idle");
        return;
      }
      setState("transcribing");
      try {
        if (blob.size > 25 * 1024 * 1024) {
          throw new Error("Recordings must be at most 25 MiB.");
        }
        const ext = mimeType.includes("ogg")
          ? "ogg"
          : mimeType.includes("mp4")
            ? "mp4"
            : "webm";
        const form = new FormData();
        form.append("file", blob, `recording.${ext}`);
        const data = await requestJson<{ text: string }>(url, {
          method: "POST",
          body: form,
          signal: AbortSignal.any([
            controller.signal,
            AbortSignal.timeout(130000),
          ]),
        });
        if (typeof data.text !== "string")
          throw new Error(
            "Speech recognition provider returned no text field.",
          );
        const text = data.text.trim();
        if (!text)
          throw new Error(
            "No speech was detected. Please try recording again.",
          );
        if (!controller.signal.aborted && activeWorkspaceId() === workspace)
          onTranscriptRef.current(text);
      } catch (err) {
        if (!controller.signal.aborted)
          setError(
            t(err instanceof Error ? err.message : "Transcription failed"),
          );
      } finally {
        if (requestRef.current === controller) {
          requestRef.current = null;
          setState("idle");
        }
      }
    };
    recorder.onerror = () => {
      clearRecordingTimer();
      controller.abort();
      if (recorder.state !== "inactive") recorder.stop();
      stream.getTracks().forEach((track) => track.stop());
      if (streamRef.current === stream) streamRef.current = null;
      if (requestRef.current === controller) {
        requestRef.current = null;
        setState("idle");
        setError(t("Recording failed. Check your microphone and try again."));
      }
    };
    try {
      recorder.start(1000);
    } catch {
      releaseStream();
      requestRef.current = null;
      setState("idle");
      setError(t("Recording is not supported in this browser."));
      return;
    }
    recorderRef.current = recorder;
    recordingTimer = setTimeout(() => {
      if (recorder.state !== "inactive") recorder.stop();
    }, 120000);
    timerRef.current = recordingTimer;
    setState("recording");
  }, [releaseStream, state, t]);

  const stop = useCallback(() => {
    const recorder = recorderRef.current;
    if (recorder && recorder.state !== "inactive") {
      recorder.stop(); // fires onstop → transcribe
    }
  }, []);

  const toggle = useCallback(() => {
    if (state === "recording") stop();
    else if (state === "idle") void start();
  }, [start, state, stop]);

  const cancel = useCallback(() => {
    requestRef.current?.abort();
    requestRef.current = null;
    if (timerRef.current) clearTimeout(timerRef.current);
    timerRef.current = null;
    const recorder = recorderRef.current;
    recorderRef.current = null;
    if (recorder && recorder.state !== "inactive") recorder.stop();
    releaseStream();
    setState("idle");
    setError(null);
  }, [releaseStream]);

  // Stop the mic if the component unmounts mid-recording.
  useEffect(() => {
    const switchWorkspace = () => {
      cancel();
      setState("idle");
      setError(null);
    };
    window.addEventListener(
      "deeptutor:before-workspace-switch",
      switchWorkspace,
    );
    return () => {
      window.removeEventListener(
        "deeptutor:before-workspace-switch",
        switchWorkspace,
      );
      cancel();
    };
  }, [cancel]);

  return { state, error, toggle, start, stop, cancel };
}
