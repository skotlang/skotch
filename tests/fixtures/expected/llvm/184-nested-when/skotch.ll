; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [14 x i8] c"very negative\00", align 1
@.str.1 = private unnamed_addr constant [9 x i8] c"negative\00", align 1
@.str.2 = private unnamed_addr constant [5 x i8] c"zero\00", align 1
@.str.3 = private unnamed_addr constant [9 x i8] c"positive\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)

define ptr @InputKt_classify(i32 %arg0) {
entry:
  %merge_2 = alloca ptr
  %merge_7 = alloca ptr
  %t0 = add i32 0, 1
  br label %bb1
bb1:
  %t1 = add i32 0, 0
  %t2 = icmp slt i32 %arg0, %t1
  br i1 %t2, label %bb2, label %bb3
bb2:
  %t3 = add i32 0, 1
  br label %bb7
bb3:
  %t4 = add i32 0, 0
  %t5 = icmp eq i32 %arg0, %t4
  br i1 %t5, label %bb4, label %bb5
bb4:
  store ptr @.str.2, ptr %merge_2
  br label %bb6
bb5:
  store ptr @.str.3, ptr %merge_2
  br label %bb6
bb6:
  %t6 = load ptr, ptr %merge_2
  ret ptr %t6
bb7:
  %t7 = add i32 0, -100
  %t8 = icmp slt i32 %arg0, %t7
  br i1 %t8, label %bb8, label %bb9
bb8:
  store ptr @.str.0, ptr %merge_7
  br label %bb10
bb9:
  store ptr @.str.1, ptr %merge_7
  br label %bb10
bb10:
  %t9 = load ptr, ptr %merge_7
  store ptr %t9, ptr %merge_2
  br label %bb6
}

define i32 @main() {
entry:
  %t0 = add i32 0, -500
  %t1 = call ptr @InputKt_classify(i32 %t0)
  call i32 @puts(ptr %t1)
  %t3 = add i32 0, -5
  %t4 = call ptr @InputKt_classify(i32 %t3)
  call i32 @puts(ptr %t4)
  %t6 = add i32 0, 0
  %t7 = call ptr @InputKt_classify(i32 %t6)
  call i32 @puts(ptr %t7)
  %t9 = add i32 0, 42
  %t10 = call ptr @InputKt_classify(i32 %t9)
  call i32 @puts(ptr %t10)
  ret i32 0
}

