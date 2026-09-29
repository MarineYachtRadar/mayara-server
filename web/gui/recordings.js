/**
 * Recordings Management UI
 *
 * Provides UI for:
 * - Recording radar data to .mrr files
 * - Playing back recorded data as virtual radars
 * - Managing recording files (list, delete)
 */

import {
  listRecordings,
  getRecordableRadars,
  startRecording,
  stopRecording,
  getRecordingStatus,
  loadPlayback,
  playPlayback,
  pausePlayback,
  stopPlayback,
  seekPlayback,
  setPlaybackSettings,
  getPlaybackStatus,
  deleteRecording,
  renameRecording,
  getRecordingDownloadUrl,
  uploadRecording,
  detectMode
} from "./api.js";
import "./app-dock.js";

// ============================================================================
// State
// ============================================================================

let selectedRadarId = null;
let selectedFile = null;
let loadedFile = null;  // Track which file is currently loaded on server
let recordingStatusInterval = null;
let playbackStatusInterval = null;

// ============================================================================
// Initialization
// ============================================================================

window.addEventListener('load', async () => {
  await detectMode();

  // Set up tab navigation
  setupTabs();

  // Load initial data for each tab
  await loadRecordableRadars();
  await loadPlaybackFiles();
  await loadFilesList();

  // Set up event handlers
  setupRecordingControls();
  setupPlaybackControls();
  setupFilesControls();
});

// ============================================================================
// Tab Navigation
// ============================================================================

function setupTabs() {
  const tabs = document.querySelectorAll('.myr_tab');
  tabs.forEach(tab => {
    tab.addEventListener('click', () => {
      // Update tab buttons
      tabs.forEach(t => t.classList.remove('myr_tab_active'));
      tab.classList.add('myr_tab_active');

      // Update tab content
      const tabId = tab.dataset.tab;
      document.querySelectorAll('.myr_tab_content').forEach(content => {
        content.classList.remove('myr_tab_visible');
      });
      document.getElementById(`tab_${tabId}`).classList.add('myr_tab_visible');

      // Refresh data when switching tabs
      if (tabId === 'playback') {
        loadPlaybackFiles();
      } else if (tabId === 'files') {
        loadFilesList();
      }
    });
  });
}

// ============================================================================
// Record Tab
// ============================================================================

async function loadRecordableRadars() {
  const container = document.getElementById('record_radar_list');

  try {
    const radars = await getRecordableRadars();

    if (radars.length === 0) {
      container.innerHTML = '<div class="myr_no_radars">No radars detected. Make sure a radar is connected and transmitting.</div>';
      return;
    }

    container.innerHTML = '';
    radars.forEach((radar, index) => {
      const option = document.createElement('label');
      option.className = 'myr_radar_option';
      option.innerHTML = `
        <input type="radio" name="record_radar" value="${radar.id}" ${index === 0 ? 'checked' : ''}>
        <span class="myr_radar_option_name">${radar.name || radar.id}</span>
        <span class="myr_radar_option_info">${radar.brand || ''}</span>
      `;

      option.querySelector('input').addEventListener('change', (e) => {
        document.querySelectorAll('.myr_radar_option').forEach(o => o.classList.remove('myr_selected'));
        option.classList.add('myr_selected');
        selectedRadarId = e.target.value;
      });

      if (index === 0) {
        option.classList.add('myr_selected');
        selectedRadarId = radar.id;
      }

      container.appendChild(option);
    });

    // Show recording controls
    document.getElementById('record_controls').style.display = 'block';

    // Check if already recording
    await updateRecordingStatus();

  } catch (err) {
    container.innerHTML = `<div class="myr_no_radars">Failed to load radars: ${err.message}</div>`;
  }
}

function setupRecordingControls() {
  const btnStart = document.getElementById('btn_start_record');
  const btnStop = document.getElementById('btn_stop_record');

  btnStart.addEventListener('click', async () => {
    if (!selectedRadarId) {
      showStatus('Please select a radar to record', 'error');
      return;
    }

    try {
      btnStart.disabled = true;
      await startRecording(selectedRadarId);
      showStatus('Recording started', 'success');

      // Start polling for status
      startRecordingStatusPolling();
    } catch (err) {
      showStatus(`Failed to start recording: ${err.message}`, 'error');
      btnStart.disabled = false;
    }
  });

  btnStop.addEventListener('click', async () => {
    try {
      btnStop.disabled = true;
      await stopRecording();
      showStatus('Recording stopped', 'success');

      // Stop polling and update UI
      stopRecordingStatusPolling();
      await updateRecordingStatus();

      // Refresh files list
      await loadPlaybackFiles();
      await loadFilesList();
    } catch (err) {
      showStatus(`Failed to stop recording: ${err.message}`, 'error');
      btnStop.disabled = false;
    }
  });
}

async function updateRecordingStatus() {
  try {
    const status = await getRecordingStatus();
    const indicator = document.getElementById('record_status_indicator');
    const statusText = document.getElementById('record_status_text');
    const btnStart = document.getElementById('btn_start_record');
    const btnStop = document.getElementById('btn_stop_record');

    // Update status indicator
    indicator.className = 'myr_status_indicator';
    if (status.state === 'recording') {
      indicator.classList.add('myr_status_recording');
      statusText.textContent = `Recording: ${status.filename || 'Unknown'}`;
      btnStart.disabled = true;
      btnStop.disabled = false;
    } else {
      indicator.classList.add('myr_status_idle');
      statusText.textContent = 'Ready to record';
      btnStart.disabled = false;
      btnStop.disabled = true;
    }

    // Update info
    document.getElementById('record_duration').textContent = formatDuration(status.durationMs || 0);
    document.getElementById('record_frames').textContent = status.frameCount || 0;
    document.getElementById('record_size').textContent = formatSize(status.sizeBytes || 0);

  } catch (err) {
    console.error('Failed to get recording status:', err);
  }
}

function startRecordingStatusPolling() {
  stopRecordingStatusPolling();
  recordingStatusInterval = setInterval(updateRecordingStatus, 1000);
}

function stopRecordingStatusPolling() {
  if (recordingStatusInterval) {
    clearInterval(recordingStatusInterval);
    recordingStatusInterval = null;
  }
}

// ============================================================================
// Playback Tab
// ============================================================================

async function loadPlaybackFiles() {
  const container = document.getElementById('playback_file_list');

  try {
    const files = await listRecordings();

    if (files.length === 0) {
      container.innerHTML = '<div class="myr_no_files">No recordings found. Record a radar session first.</div>';
      document.getElementById('playback_controls').style.display = 'none';
      return;
    }

    container.innerHTML = '';
    files.forEach((file, index) => {
      const option = document.createElement('label');
      option.className = 'myr_file_option';
      option.innerHTML = `
        <input type="radio" name="playback_file" value="${file.filename}">
        <div class="myr_file_option_info">
          <span class="myr_file_option_name">${file.filename}</span>
          <span class="myr_file_option_meta">
            <span>${formatDuration(file.durationMs)}</span>
            <span>${formatSize(file.size)}</span>
          </span>
        </div>
      `;

      option.querySelector('input').addEventListener('change', async (e) => {
        document.querySelectorAll('.myr_file_option').forEach(o => o.classList.remove('myr_selected'));
        option.classList.add('myr_selected');
        const newSelection = e.target.value;

        // If a different file is currently loaded/playing, stop it first
        if (loadedFile && loadedFile !== newSelection) {
          stopPlaybackStatusPolling();
          try {
            await stopPlayback();
          } catch (err) {
            // Ignore stop errors
            console.log('Stop on file change (expected):', err.message);
          }
          loadedFile = null;
          // Reset UI to show ready state
          await updatePlaybackStatus();
        }

        selectedFile = newSelection;
        // Enable the play button when a file is selected
        document.getElementById('btn_play').disabled = false;
      });

      container.appendChild(option);
    });

    // Show playback controls section (but keep buttons disabled until file loaded)
    document.getElementById('playback_controls').style.display = 'block';

    // Check current playback status
    await updatePlaybackStatus();

  } catch (err) {
    container.innerHTML = `<div class="myr_no_files">Failed to load recordings: ${err.message}</div>`;
  }
}

async function loadSelectedFile() {
  if (!selectedFile) return;

  try {
    showStatus('Loading recording...', 'info');
    const status = await loadPlayback(selectedFile);

    // Track which file is loaded
    loadedFile = selectedFile;

    document.getElementById('playback_filename').textContent = selectedFile;

    // Enable controls
    document.getElementById('btn_play').disabled = false;
    document.getElementById('playback_timeline').disabled = false;

    // Apply current UI settings to the loaded playback
    const loopCheckbox = document.getElementById('playback_loop');
    const activeSpeedBtn = document.querySelector('.myr_speed_button.myr_speed_active');
    const speed = activeSpeedBtn ? parseFloat(activeSpeedBtn.dataset.speed) : 1.0;
    await setPlaybackSettings({ loopPlayback: loopCheckbox.checked, speed });

    // Update view radar link
    const viewLink = document.getElementById('view_radar_link');
    if (status.radarId) {
      viewLink.href = `viewer.html?id=${encodeURIComponent(status.radarId)}`;
      viewLink.style.display = 'inline-block';
    }

    await updatePlaybackStatus();
    showStatus('Recording loaded', 'success');

  } catch (err) {
    loadedFile = null;  // Clear on failure
    showStatus(`Failed to load recording: ${err.message}`, 'error');
  }
}

function setupPlaybackControls() {
  const btnPlay = document.getElementById('btn_play');
  const btnPause = document.getElementById('btn_pause');
  const btnStop = document.getElementById('btn_stop_playback');
  const timeline = document.getElementById('playback_timeline');
  const loopCheckbox = document.getElementById('playback_loop');
  const speedButtons = document.querySelectorAll('.myr_speed_button');

  btnPlay.addEventListener('click', async () => {
    if (!selectedFile) {
      showStatus('Please select a recording first', 'error');
      return;
    }

    // Disable buttons during transition to prevent double-clicks
    btnPlay.disabled = true;
    btnPause.disabled = true;
    btnStop.disabled = true;

    try {
      // Always stop any existing playback before loading a different file
      // This ensures clean state even if loadedFile tracking is out of sync
      if (loadedFile !== selectedFile) {
        // Stop polling during transition
        stopPlaybackStatusPolling();

        // Try to stop any existing playback (server-side handles cleanup)
        try {
          await stopPlayback();
        } catch (e) {
          // Ignore stop errors - might already be stopped or nothing loaded
          console.log('Stop during switch (expected):', e.message);
        }
        loadedFile = null;

        // Small delay to let server clean up
        await new Promise(resolve => setTimeout(resolve, 100));

        // Load the new file
        await loadSelectedFile();
      }

      await playPlayback();
      startPlaybackStatusPolling();
    } catch (err) {
      // If playback has stopped/ended, reload and retry
      if (err.message.includes('410') || err.message.includes('Gone') || err.message.includes('stopped')) {
        try {
          loadedFile = null;
          await loadSelectedFile();
          await playPlayback();
          startPlaybackStatusPolling();
        } catch (retryErr) {
          showStatus(`Failed to restart playback: ${retryErr.message}`, 'error');
        }
      } else {
        showStatus(`Failed to play: ${err.message}`, 'error');
      }
    }

    // Re-enable based on current state (updatePlaybackStatus will set correct state)
    await updatePlaybackStatus();
  });

  btnPause.addEventListener('click', async () => {
    try {
      await pausePlayback();
      await updatePlaybackStatus();
    } catch (err) {
      showStatus(`Failed to pause: ${err.message}`, 'error');
    }
  });

  btnStop.addEventListener('click', async () => {
    try {
      await stopPlayback();
      stopPlaybackStatusPolling();
      loadedFile = null;  // Clear loaded file on stop
      await updatePlaybackStatus();

      // Hide view link
      document.getElementById('view_radar_link').style.display = 'none';
    } catch (err) {
      showStatus(`Failed to stop: ${err.message}`, 'error');
    }
  });

  // Timeline seeking
  let seeking = false;
  timeline.addEventListener('input', () => {
    seeking = true;
  });

  timeline.addEventListener('change', async (e) => {
    try {
      const positionMs = parseInt(e.target.value);
      await seekPlayback(positionMs);
    } catch (err) {
      console.error('Seek failed:', err);
    }
    seeking = false;
  });

  // Loop checkbox - only send to server if a recording is loaded
  loopCheckbox.addEventListener('change', async (e) => {
    if (!loadedFile) return;  // Don't call API if nothing loaded
    try {
      await setPlaybackSettings({ loopPlayback: e.target.checked });
    } catch (err) {
      console.error('Failed to update loop setting:', err);
    }
  });

  // Speed buttons - update UI immediately, only call API if loaded
  speedButtons.forEach(btn => {
    btn.addEventListener('click', async () => {
      const speed = parseFloat(btn.dataset.speed);
      // Update UI immediately
      speedButtons.forEach(b => b.classList.remove('myr_speed_active'));
      btn.classList.add('myr_speed_active');
      // Only send to server if a recording is loaded
      if (!loadedFile) return;
      try {
        await setPlaybackSettings({ speed });
      } catch (err) {
        console.error('Failed to update speed:', err);
      }
    });
  });
}

async function updatePlaybackStatus() {
  try {
    const status = await getPlaybackStatus();
    const indicator = document.getElementById('playback_status_indicator');
    const statusText = document.getElementById('playback_status_text');
    const btnPlay = document.getElementById('btn_play');
    const btnPause = document.getElementById('btn_pause');
    const btnStop = document.getElementById('btn_stop_playback');
    const timeline = document.getElementById('playback_timeline');

    // Update status indicator
    indicator.className = 'myr_status_indicator';
    switch (status.state) {
      case 'playing':
        indicator.classList.add('myr_status_playing');
        statusText.textContent = 'Playing';
        btnPlay.disabled = true;
        btnPause.disabled = false;
        btnStop.disabled = false;
        break;
      case 'paused':
        indicator.classList.add('myr_status_paused');
        statusText.textContent = 'Paused';
        btnPlay.disabled = false;
        btnPause.disabled = true;
        btnStop.disabled = false;
        break;
      case 'loaded':
        indicator.classList.add('myr_status_loaded');
        statusText.textContent = 'Loaded';
        btnPlay.disabled = false;
        btnPause.disabled = true;
        btnStop.disabled = false;
        break;
      default:
        indicator.classList.add('myr_status_idle');
        statusText.textContent = selectedFile ? 'Ready' : 'No recording selected';
        // Keep play enabled if a file is selected
        btnPlay.disabled = !selectedFile;
        btnPause.disabled = true;
        btnStop.disabled = true;
    }

    // Update position info
    const positionText = `${formatDuration(status.positionMs || 0)} / ${formatDuration(status.durationMs || 0)}`;
    document.getElementById('playback_position').textContent = positionText;
    document.getElementById('playback_frame').textContent = `${status.frame || 0} / ${status.frameCount || 0}`;

    // Update timeline
    if (status.durationMs > 0) {
      timeline.max = status.durationMs;
      timeline.value = status.positionMs || 0;
      timeline.disabled = false;
    }

    // Only sync loop/speed with server if a file is actually loaded
    // This preserves user's UI selections before loading a file
    if (status.filename) {
      document.getElementById('playback_loop').checked = status.loopPlayback || false;

      const speedButtons = document.querySelectorAll('.myr_speed_button');
      speedButtons.forEach(btn => {
        const speed = parseFloat(btn.dataset.speed);
        btn.classList.toggle('myr_speed_active', Math.abs(speed - (status.speed || 1)) < 0.01);
      });
    }

    // Update view radar link visibility
    const viewLink = document.getElementById('view_radar_link');
    if (status.radarId && status.state !== 'idle' && status.state !== 'stopped') {
      viewLink.href = `viewer.html?id=${encodeURIComponent(status.radarId)}`;
      viewLink.style.display = 'inline-block';
    }

    // Update filename display
    if (status.filename) {
      document.getElementById('playback_filename').textContent = status.filename;
    }

  } catch (err) {
    console.error('Failed to get playback status:', err);
  }
}

function startPlaybackStatusPolling() {
  stopPlaybackStatusPolling();
  playbackStatusInterval = setInterval(updatePlaybackStatus, 500);
}

function stopPlaybackStatusPolling() {
  if (playbackStatusInterval) {
    clearInterval(playbackStatusInterval);
    playbackStatusInterval = null;
  }
}

// ============================================================================
// Files Tab
// ============================================================================

async function loadFilesList() {
  const container = document.getElementById('files_list');

  try {
    const files = await listRecordings();

    if (files.length === 0) {
      container.innerHTML = '<div class="myr_no_files">No recordings found.</div>';
      return;
    }

    container.innerHTML = '';
    files.forEach(file => {
      const row = document.createElement('div');
      row.className = 'myr_file_row';
      row.innerHTML = `
        <div class="myr_file_info">
          <span class="myr_file_name">${file.filename}</span>
          <span class="myr_file_details">
            <span>${formatDuration(file.durationMs)}</span>
            <span>${formatSize(file.size)}</span>
            <span>${file.frameCount || 0} frames</span>
          </span>
        </div>
        <span class="myr_file_actions">
          <button class="myr_file_action myr_file_action_play" data-file="${file.filename}">Play</button>
          <button class="myr_file_action myr_file_action_rename" data-file="${file.filename}">Rename</button>
          <a href="${getRecordingDownloadUrl(file.filename)}" download class="myr_file_action myr_file_action_download" title="Download compressed (.mrr.gz)">Download</a>
          <button class="myr_file_action myr_file_action_delete" data-file="${file.filename}">Delete</button>
        </span>
      `;
      container.appendChild(row);
    });

    // Add event handlers
    container.querySelectorAll('.myr_file_action_play').forEach(btn => {
      btn.addEventListener('click', () => playFile(btn.dataset.file));
    });

    container.querySelectorAll('.myr_file_action_rename').forEach(btn => {
      btn.addEventListener('click', () => showRenameModal(btn.dataset.file));
    });

    container.querySelectorAll('.myr_file_action_delete').forEach(btn => {
      btn.addEventListener('click', () => confirmDeleteFile(btn.dataset.file));
    });

  } catch (err) {
    container.innerHTML = `<div class="myr_no_files">Failed to load files: ${err.message}</div>`;
  }
}

function setupFilesControls() {
  document.getElementById('btn_refresh_files').addEventListener('click', loadFilesList);
  setupUpload();
}

function setupUpload() {
  const zone = document.getElementById('upload_zone');
  const input = document.getElementById('file_input');

  // Click to browse
  zone.addEventListener('click', () => input.click());

  // Drag and drop
  zone.addEventListener('dragover', (e) => {
    e.preventDefault();
    zone.classList.add('myr_dragover');
  });

  zone.addEventListener('dragleave', () => {
    zone.classList.remove('myr_dragover');
  });

  zone.addEventListener('drop', (e) => {
    e.preventDefault();
    zone.classList.remove('myr_dragover');
    if (e.dataTransfer.files.length > 0) {
      handleFileUpload(e.dataTransfer.files[0]);
    }
  });

  // File input change
  input.addEventListener('change', () => {
    if (input.files.length > 0) {
      handleFileUpload(input.files[0]);
      input.value = ''; // Reset for next upload
    }
  });
}

async function handleFileUpload(file) {
  // Validate file extension
  const name = file.name.toLowerCase();
  if (!name.endsWith('.mrr') && !name.endsWith('.mrr.gz') && !name.endsWith('.gz')) {
    showStatus('Invalid file type. Please upload .mrr or .mrr.gz files.', 'error');
    return;
  }

  try {
    showStatus('Uploading...', 'info');
    const result = await uploadRecording(file);
    showStatus(`Uploaded: ${result.filename}`, 'success');

    // Refresh file lists
    await loadFilesList();
    await loadPlaybackFiles();
  } catch (err) {
    showStatus(`Upload failed: ${err.message}`, 'error');
  }
}

async function playFile(filename) {
  // Switch to playback tab
  document.querySelectorAll('.myr_tab').forEach(t => t.classList.remove('myr_tab_active'));
  document.querySelector('.myr_tab[data-tab="playback"]').classList.add('myr_tab_active');
  document.querySelectorAll('.myr_tab_content').forEach(c => c.classList.remove('myr_tab_visible'));
  document.getElementById('tab_playback').classList.add('myr_tab_visible');

  // Select and load the file
  selectedFile = filename;
  await loadPlaybackFiles();

  // Find and select the radio button
  const radio = document.querySelector(`input[name="playback_file"][value="${filename}"]`);
  if (radio) {
    radio.checked = true;
    radio.closest('.myr_file_option').classList.add('myr_selected');
  }

  await loadSelectedFile();
}

async function confirmDeleteFile(filename) {
  if (!confirm(`Delete recording "${filename}"?\n\nThis action cannot be undone.`)) {
    return;
  }

  try {
    await deleteRecording(filename);
    showStatus('Recording deleted', 'success');
    await loadFilesList();
    await loadPlaybackFiles();
  } catch (err) {
    showStatus(`Failed to delete: ${err.message}`, 'error');
  }
}

// ============================================================================
// Rename Modal
// ============================================================================

function showRenameModal(filename) {
  // Extract base name without extension
  const ext = filename.endsWith('.mrr.gz') ? '.mrr.gz' : '.mrr';
  const baseName = filename.replace(/\.mrr(\.gz)?$/, '');

  // Create modal overlay
  const overlay = document.createElement('div');
  overlay.className = 'myr_modal_overlay';
  overlay.innerHTML = `
    <div class="myr_modal">
      <div class="myr_modal_header">Rename Recording</div>
      <div class="myr_modal_body">
        <label class="myr_modal_label">New filename:</label>
        <input type="text" class="myr_modal_input" id="rename_input" value="${baseName}">
        <div class="myr_modal_hint">
          Add descriptive info like range, conditions, etc.<br>
          Extension (${ext}) will be added automatically.
        </div>
      </div>
      <div class="myr_modal_footer">
        <button class="myr_modal_button myr_modal_button_cancel" id="rename_cancel">Cancel</button>
        <button class="myr_modal_button myr_modal_button_save" id="rename_save">Rename</button>
      </div>
    </div>
  `;

  document.body.appendChild(overlay);

  const input = document.getElementById('rename_input');
  const btnCancel = document.getElementById('rename_cancel');
  const btnSave = document.getElementById('rename_save');

  // Focus and select input
  input.focus();
  input.select();

  // Close modal
  function closeModal() {
    overlay.remove();
  }

  // Save handler
  async function saveRename() {
    const newBaseName = input.value.trim();
    if (!newBaseName) {
      showStatus('Filename cannot be empty', 'error');
      return;
    }

    const newFilename = newBaseName + ext;
    if (newFilename === filename) {
      closeModal();
      return;
    }

    try {
      await renameRecording(filename, newFilename);
      showStatus(`Renamed to: ${newFilename}`, 'success');
      closeModal();
      await loadFilesList();
      await loadPlaybackFiles();
    } catch (err) {
      showStatus(`Rename failed: ${err.message}`, 'error');
    }
  }

  // Event handlers
  btnCancel.addEventListener('click', closeModal);
  btnSave.addEventListener('click', saveRename);

  // Close on overlay click (outside modal)
  overlay.addEventListener('click', (e) => {
    if (e.target === overlay) closeModal();
  });

  // Enter to save, Escape to cancel
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') saveRename();
    if (e.key === 'Escape') closeModal();
  });
}

// ============================================================================
// Utility Functions
// ============================================================================

function formatDuration(ms) {
  if (!ms || ms <= 0) return '0:00';
  const seconds = Math.floor(ms / 1000);
  const minutes = Math.floor(seconds / 60);
  const hours = Math.floor(minutes / 60);

  if (hours > 0) {
    return `${hours}:${String(minutes % 60).padStart(2, '0')}:${String(seconds % 60).padStart(2, '0')}`;
  }
  return `${minutes}:${String(seconds % 60).padStart(2, '0')}`;
}

function formatSize(bytes) {
  if (!bytes || bytes <= 0) return '0 KB';
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

function showStatus(message, type = 'info') {
  const el = document.getElementById('status_message');
  el.textContent = message;
  el.className = `myr_status_message myr_${type}`;
  el.style.display = 'block';

  setTimeout(() => {
    el.style.display = 'none';
  }, 3000);
}
