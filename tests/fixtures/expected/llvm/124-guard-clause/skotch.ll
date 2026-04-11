; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @InputKt_clamp(i32 %arg0, i32 %arg1, i32 %arg2) {
entry:
  %t0 = icmp slt i32 %arg0, %arg1
  br i1 %t0, label %bb1, label %bb2
bb1:
  ret i32 %arg1
bb2:
  br label %bb3
bb3:
  %t1 = icmp sgt i32 %arg0, %arg2
  br i1 %t1, label %bb4, label %bb5
bb4:
  ret i32 %arg2
bb5:
  br label %bb6
bb6:
  ret i32 %arg0
}

define i32 @main() {
entry:
  %t0 = add i32 0, 5
  %t1 = add i32 0, 1
  %t2 = add i32 0, 10
  %t3 = call i32 @InputKt_clamp(i32 %t0, i32 %t1, i32 %t2)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t3)
  %t5 = add i32 0, -3
  %t6 = add i32 0, 0
  %t7 = add i32 0, 100
  %t8 = call i32 @InputKt_clamp(i32 %t5, i32 %t6, i32 %t7)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t8)
  %t10 = add i32 0, 999
  %t11 = add i32 0, 0
  %t12 = add i32 0, 100
  %t13 = call i32 @InputKt_clamp(i32 %t10, i32 %t11, i32 %t12)
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t13)
  ret i32 0
}

