"use strict";

/* ------------------------------------------------------------------ */
/* Pictures that go with the next message                              */
/* ------------------------------------------------------------------ */

function readImageFile(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result || ""));
    reader.onerror = () => reject(new Error("the picture could not be read"));
    reader.readAsDataURL(file);
  });
}

async function addImageFiles(files) {
  for (const file of files) {
    if (!file.type || !file.type.startsWith("image/")) {
      addLine("notice", file.name + " is not a picture.");
      continue;
    }
    if (file.size > 12 * 1024 * 1024) {
      addLine("error", file.name + " is larger than 12 MB.");
      continue;
    }
    if (state.images.length >= 4) {
      addLine("notice", "four pictures is the limit for one message.");
      break;
    }
    try {
      const dataUrl = await readImageFile(file);
      const data = dataUrl.slice(dataUrl.indexOf(",") + 1);
      state.images.push({ name: file.name, mediaType: file.type, data: data });
    } catch (e) {
      addLine("error", e.message);
    }
  }
  renderAttachments();
}

function renderAttachments() {
  el.attachments.innerHTML = "";
  el.attachments.hidden = state.images.length === 0;
  state.images.forEach((image, index) => {
    const chip = document.createElement("div");
    chip.className = "attachment";
    const preview = document.createElement("img");
    preview.src = "data:" + image.mediaType + ";base64," + image.data;
    preview.alt = image.name;
    const label = document.createElement("span");
    label.textContent = image.name;
    const remove = document.createElement("button");
    remove.type = "button";
    remove.textContent = "×";
    remove.setAttribute("aria-label", "remove " + image.name);
    remove.addEventListener("click", () => {
      state.images.splice(index, 1);
      renderAttachments();
    });
    chip.appendChild(preview);
    chip.appendChild(label);
    chip.appendChild(remove);
    el.attachments.appendChild(chip);
  });
}


async function captureLiveFrame(kind) {
  if (!navigator.mediaDevices) return null;
  let stream = null;
  try {
    if (kind === "camera") {
      stream = await navigator.mediaDevices.getUserMedia({ video: true });
    } else if (kind === "screen") {
      stream = await navigator.mediaDevices.getDisplayMedia({ video: true });
    } else {
      return null;
    }
    const video = document.createElement("video");
    video.srcObject = stream;
    video.muted = true;
    await video.play();
    await new Promise((resolve) => {
      if (video.readyState >= 2) resolve();
      else video.onloadeddata = () => resolve();
    });
    const canvas = document.createElement("canvas");
    canvas.width = video.videoWidth || 640;
    canvas.height = video.videoHeight || 480;
    canvas.getContext("2d").drawImage(video, 0, 0, canvas.width, canvas.height);
    stream.getTracks().forEach((track) => track.stop());
    const dataUrl = canvas.toDataURL("image/png");
    const data = dataUrl.slice(dataUrl.indexOf(",") + 1);
    return { name: kind + "-frame.png", mediaType: "image/png", data: data };
  } catch (e) {
    if (stream) stream.getTracks().forEach((track) => track.stop());
    if (typeof addLine === "function") {
      addLine("error", "could not capture the " + kind + ": " + (e && e.message ? e.message : e));
    }
    return null;
  }
}
