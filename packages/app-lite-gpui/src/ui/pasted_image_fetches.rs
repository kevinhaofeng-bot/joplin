//! Web images pasted into notes are fetched here, by the library window
//! rather than a note's editor session: closing or switching the note does
//! not stop them, and one that is not finished when the app quits resumes
//! from the library's job list at the next launch.

use super::*;
use crate::app::note_session::{
    pasted_image_job_given_up, store_pasted_image_outside_session,
    unmark_pasted_image_outside_session,
};
use app_lite_core::PastedImageJob;
use futures::StreamExt as _;

pub(super) struct PastedImageFetch {
    id: u64,
    jobs: Vec<ResourceId>,
    download: crate::net::pasted_images::PasteDownload,
    _results: Task<()>,
}

impl LibraryShell {
    pub(super) fn resume_pasted_image_jobs(
        &mut self,
        note: Option<&NoteId>,
        cx: &mut Context<Self>,
    ) {
        let repository = self.model.read(cx).repository();
        match repository.pasted_image_jobs(note) {
            Ok(jobs) => self.fetch_pasted_images(jobs, cx),
            Err(error) => {
                self.resource_notice = Some(format!("无法继续获取粘贴的图片：{error}"));
                cx.notify();
            }
        }
    }

    /// Starts the jobs not already being fetched.
    pub(super) fn fetch_pasted_images(
        &mut self,
        jobs: Vec<PastedImageJob>,
        cx: &mut Context<Self>,
    ) {
        let jobs: Vec<_> = jobs
            .into_iter()
            .filter(|job| {
                !self
                    .pasted_image_fetches
                    .iter()
                    .any(|fetch| fetch.jobs.contains(&job.id))
            })
            .collect();
        if jobs.is_empty() {
            return;
        }
        let (sender, mut results) = futures::channel::mpsc::unbounded();
        let sender = std::sync::Mutex::new(sender);
        #[cfg(test)]
        let delivered = std::sync::Arc::clone(&self.pasted_image_results_for_test);
        let download = crate::net::pasted_images::start_pasted_image_downloads(
            jobs.iter()
                .map(|job| (job.url.clone(), job.alt.clone()))
                .collect(),
            crate::net::pasted_images::PasteDownloadLimits::default(),
            move |index, result| {
                let _ = sender
                    .lock()
                    .expect("paste result sender")
                    .unbounded_send((index, result));
                #[cfg(test)]
                {
                    let (count, changed) = &*delivered;
                    *count.lock().expect("result count") += 1;
                    changed.notify_all();
                }
            },
        );
        self.next_pasted_image_fetch += 1;
        let id = self.next_pasted_image_fetch;
        let ids = jobs.iter().map(|job| job.id.clone()).collect();
        let results = cx.spawn(async move |this, cx| {
            while let Some((index, result)) = results.next().await {
                let Some(job) = jobs.get(index).cloned() else {
                    continue;
                };
                if this
                    .update(cx, |shell, cx| shell.finish_pasted_image(job, result, cx))
                    .is_err()
                {
                    return;
                }
            }
            let _ = this.update(cx, |shell, _| {
                shell.pasted_image_fetches.retain(|fetch| fetch.id != id);
            });
        });
        self.pasted_image_fetches.push(PastedImageFetch {
            id,
            jobs: ids,
            download,
            _results: results,
        });
    }

    fn finish_pasted_image(
        &mut self,
        job: PastedImageJob,
        result: Result<crate::native_editor::images::ResourceImport, String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.note_session.clone()
            && session.read(cx).shows_pasted_image(&job)
        {
            session.update(cx, |session, session_cx| {
                session.store_pasted_image(&job, result, session_cx)
            });
            return;
        }
        let repository = self.model.read(cx).repository();
        let outcome =
            result.and_then(|import| store_pasted_image_outside_session(&repository, &job, import));
        if let Err(error) = outcome {
            self.resource_notice = Some(if pasted_image_job_given_up(&repository, &job, &error) {
                let _ = unmark_pasted_image_outside_session(&repository, &job);
                "1 张图片未能获取，已保留为指向原图的链接".to_owned()
            } else {
                "1 张图片暂时未能获取，先保留为指向原图的链接；重新启动时会重试".to_owned()
            });
            cx.notify();
        }
    }

    /// Blocks until `count` fetch results in all have been delivered (they
    /// land once the executor runs).
    #[cfg(test)]
    pub(crate) fn wait_for_pasted_image_results_for_test(&self, count: usize) {
        let (delivered, changed) = &*self.pasted_image_results_for_test;
        let delivered = delivered.lock().expect("result count");
        let (delivered, timeout) = changed
            .wait_timeout_while(delivered, std::time::Duration::from_secs(30), |delivered| {
                *delivered < count
            })
            .expect("result count");
        assert!(
            !timeout.timed_out(),
            "only {} of {count} pasted-image results arrived within 30 s",
            *delivered
        );
    }

    #[cfg(test)]
    pub(crate) fn wait_for_pasted_image_downloads_for_test(&self) {
        for fetch in &self.pasted_image_fetches {
            assert!(
                fetch.download.wait_for(std::time::Duration::from_secs(30)),
                "pasted-image fetch {} still running after 30 s: {:?}",
                fetch.id,
                fetch.jobs
            );
        }
    }
}
