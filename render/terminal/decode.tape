Type "pcodec probe clip.mp4"
Enter
Wait@10s /^\$\s*$/
Sleep 1s
Type "pcodec decode clip.mp4 --route cpu"
Enter
Wait@60s /^\$\s*$/
Sleep 1s
Type "pcodec decode clip.mp4 --route gpu"
Enter
Wait@60s /^\$\s*$/
Sleep 1s
Type "pcodec encode clip.mp4 out.mp4 --codec hevc --route gpu"
Enter
Wait@120s /^\$\s*$/
Sleep 1s
Type "ls -sh out.mp4"
Enter
Wait@10s /^\$\s*$/
Sleep 4s
