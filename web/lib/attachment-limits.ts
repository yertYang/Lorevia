import {
  DEFAULT_MAX_ATTACHMENT_BYTES,
  DEFAULT_MAX_TOTAL_ATTACHMENT_BYTES,
} from "@/lib/doc-attachments";

export interface AttachmentLimits {
  maxFileBytes: number;
  maxTotalBytes: number;
}

// Client-side fallback while the legacy attachment-settings API is absent.
// Server upload limits are enforced by their individual native endpoints.
export const DEFAULT_ATTACHMENT_LIMITS: AttachmentLimits = {
  maxFileBytes: DEFAULT_MAX_ATTACHMENT_BYTES,
  maxTotalBytes: DEFAULT_MAX_TOTAL_ATTACHMENT_BYTES,
};

export function useAttachmentLimits(): AttachmentLimits {
  return DEFAULT_ATTACHMENT_LIMITS;
}
