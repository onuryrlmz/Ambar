// What untrusted names look like on screen. Display only: actions keep the exact name.

const UNSAFE_NAME = /[\u0000-\u001f\u007f-\u009f\u061C\u200E\u200F\u202A-\u202E\u2066-\u2069]/g;

const UNSAFE_TEXT = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/g;

/** A name from an archive is untrusted: a bidi override can make `x.sh` read as a text file. */
export const safeName = (name: string): string => name.replace(UNSAFE_NAME, "\uFFFD");

/** For several lines of untrusted text, such as an archive comment: line breaks and tabs stay. */
export const safeText = (text: string): string => text.replace(UNSAFE_TEXT, "\uFFFD");
